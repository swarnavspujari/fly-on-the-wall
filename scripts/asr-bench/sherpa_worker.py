"""Bench-only sherpa-onnx ASR worker for the accuracy harness.

Driven by src-tauri/tests/accuracy_harness/bench_engines.rs over stdin/stdout
(one JSON object per line), so a model loads ONCE per channel instead of
once per VAD batch. Never used by the app.

Run with the bench venv's interpreter in isolated mode (-I), e.g.
  venv\\Scripts\\python.exe -I scripts\\asr-bench\\sherpa_worker.py \\
      --kind online-transducer --model-dir <dir> --threads 8

Kinds:
  online-transducer   NeMo cache-aware streaming transducer (Nemotron
                      speech-streaming; the chunk size is baked into the
                      export). Requests: {"stream": wav, "feed_ms": 160}
                      feeds the file in capture-sized buffers like a live
                      call and reports partial-emission latency; {"wav": wav}
                      decodes the same way without latency bookkeeping.
  offline-transducer  NeMo offline transducer / TDT (whole-batch decode).
  cohere              Cohere Transcribe (encoder-decoder). {"wav": wav}

Every reply: {"ok": true, "tokens": [...], "starts": [s...],
"durations": [s...] | null, "text": "...", "decode_secs": f, ...} or
{"ok": false, "error": "..."}. Times are seconds relative to the wav.
"""

import argparse
import json
import os
import sys
import time
import wave

import numpy as np
import sherpa_onnx


def read_wav(path):
    with wave.open(path, "rb") as w:
        if w.getsampwidth() != 2:
            raise ValueError(f"{path}: expected 16-bit PCM")
        rate = w.getframerate()
        ch = w.getnchannels()
        raw = w.readframes(w.getnframes())
    x = np.frombuffer(raw, dtype=np.int16).astype(np.float32) / 32768.0
    if ch > 1:
        x = x.reshape(-1, ch).mean(axis=1)
    return x, rate


def find(model_dir, *names):
    """First existing file among `names` (int8 preferred by the caller's order)."""
    for n in names:
        p = os.path.join(model_dir, n)
        if os.path.exists(p):
            return p
    raise FileNotFoundError(f"none of {names} in {model_dir}")


def build(args):
    d = args.model_dir
    tokens = find(d, "tokens.txt")
    if args.kind == "online-transducer":
        return sherpa_onnx.OnlineRecognizer.from_transducer(
            tokens=tokens,
            encoder=find(d, "encoder.int8.onnx", "encoder.onnx"),
            decoder=find(d, "decoder.int8.onnx", "decoder.onnx"),
            joiner=find(d, "joiner.int8.onnx", "joiner.onnx"),
            num_threads=args.threads,
            sample_rate=16000,
            feature_dim=args.feature_dim,
            decoding_method="greedy_search",
            model_type=args.model_type,
            provider=args.provider,
        )
    if args.kind == "offline-transducer":
        return sherpa_onnx.OfflineRecognizer.from_transducer(
            tokens=tokens,
            encoder=find(d, "encoder.int8.onnx", "encoder.onnx"),
            decoder=find(d, "decoder.int8.onnx", "decoder.onnx"),
            joiner=find(d, "joiner.int8.onnx", "joiner.onnx"),
            num_threads=args.threads,
            sample_rate=16000,
            feature_dim=args.feature_dim,
            decoding_method="greedy_search",
            model_type=args.model_type or "nemo_transducer",
            provider=args.provider,
        )
    if args.kind == "cohere":
        return sherpa_onnx.OfflineRecognizer.from_cohere_transcribe(
            encoder=find(d, "encoder.int8.onnx", "encoder.onnx"),
            decoder=find(d, "decoder.int8.onnx", "decoder.onnx"),
            tokens=tokens,
            num_threads=args.threads,
            language="en",
            use_punct=True,
            use_itn=True,
            provider=args.provider,
        )
    raise ValueError(f"unknown kind {args.kind}")


def offline_decode(rec, wav):
    x, rate = read_wav(wav)
    t0 = time.perf_counter()
    s = rec.create_stream()
    s.accept_waveform(rate, x)
    rec.decode_stream(s)
    r = s.result
    secs = time.perf_counter() - t0
    durations = list(r.durations) if len(getattr(r, "durations", [])) else None
    return {
        "ok": True,
        "text": r.text,
        "tokens": list(r.tokens),
        "starts": [float(t) for t in r.timestamps],
        "durations": durations,
        "segment_timestamps": [float(t) for t in getattr(r, "segment_timestamps", [])],
        "segment_texts": list(getattr(r, "segment_texts", [])),
        "decode_secs": secs,
        "audio_secs": len(x) / rate,
    }


# Leading silence before the audio (token times are shifted back by it).
# The sherpa-onnx CLI pads 0.5 s by default (>= 1.13.5), but on meeting
# audio that padding cost punctuation and a word on a 60 s AMI clip, while
# the feed size had no effect at all — so no padding here.
LEFT_PAD_S = 0.0


def online_decode(rec, wav, feed_ms, latency, left_pad_s=LEFT_PAD_S):
    """Feed the file in `feed_ms` capture buffers like a live call (the
    model's own chunk — 1.12 s for the 1120ms export — fires whenever
    enough audio has arrived). No endpoint resets: one continuous stream
    keeps absolute token times. With `latency`, record for every newly
    emitted token the audio position at which it first appeared and the
    compute time of that buffer."""
    x, rate = read_wav(wav)
    pad = int(rate * left_pad_s)
    x = np.concatenate([np.zeros(pad, dtype=np.float32), x])
    step = max(1, int(rate * feed_ms / 1000))
    s = rec.create_stream()
    seen = 0
    emits = []  # (token_start_s, audio_fed_s, chunk_compute_s)
    chunk_secs = []
    t_start = time.perf_counter()
    for i in range(0, len(x), step):
        c0 = time.perf_counter()
        s.accept_waveform(rate, x[i : i + step])
        while rec.is_ready(s):
            rec.decode_stream(s)
        dt = time.perf_counter() - c0
        chunk_secs.append(dt)
        if latency:
            r = rec.get_result_all(s)
            ts = list(r.timestamps)
            if len(ts) > seen:
                fed = (min(len(x), i + step) - pad) / rate
                for t in ts[seen:]:
                    emits.append((float(t) + float(r.start_time) - left_pad_s, fed, dt))
                seen = len(ts)
    # flush the right context: a live stream gets this as trailing silence
    s.accept_waveform(rate, np.zeros(int(rate * 2), dtype=np.float32))
    s.input_finished()
    while rec.is_ready(s):
        rec.decode_stream(s)
    secs = time.perf_counter() - t_start
    r = rec.get_result_all(s)
    off = float(r.start_time) - left_pad_s
    out = {
        "ok": True,
        "text": r.text,
        "tokens": list(r.tokens),
        "starts": [float(t) + off for t in r.timestamps],
        "durations": None,
        "decode_secs": secs,
        "audio_secs": (len(x) - pad) / rate,
        "feed_ms": feed_ms,
        "chunk_compute_p50_s": float(np.percentile(chunk_secs, 50)) if chunk_secs else None,
        "chunk_compute_p95_s": float(np.percentile(chunk_secs, 95)) if chunk_secs else None,
        "chunk_compute_max_s": float(max(chunk_secs)) if chunk_secs else None,
    }
    if latency:
        out["emits"] = emits
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--kind", required=True)
    ap.add_argument("--model-dir", required=True)
    ap.add_argument("--threads", type=int, default=4)
    ap.add_argument("--provider", default="cpu")
    ap.add_argument("--model-type", default="")
    ap.add_argument("--feature-dim", type=int, default=80)
    args = ap.parse_args()
    t0 = time.perf_counter()
    rec = build(args)
    print(json.dumps({"ready": True, "load_secs": time.perf_counter() - t0}), flush=True)
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
            if "stream" in req:
                rep = online_decode(
                    rec, req["stream"], int(req.get("feed_ms", 160)), True,
                    float(req.get("left_pad_s", LEFT_PAD_S)))
            elif args.kind == "online-transducer":
                rep = online_decode(
                    rec, req["wav"], int(req.get("feed_ms", 160)), False,
                    float(req.get("left_pad_s", LEFT_PAD_S)))
            else:
                rep = offline_decode(rec, req["wav"])
        except Exception as e:  # noqa: BLE001 - reported to the harness
            rep = {"ok": False, "error": f"{type(e).__name__}: {e}"}
        print(json.dumps(rep), flush=True)


if __name__ == "__main__":
    main()
