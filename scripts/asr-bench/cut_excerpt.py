"""Cut a scoring excerpt: audio + VTT reference cropped to the same window.

Window edges snap to silent gaps between reference cues (>= GAP s with no
cue active), so no half-utterance leaks in on either side.
usage: python -I cut_excerpt.py <wav> <vtt> <out_dir> <start_s> <len_s>
"""
import json
import re
import subprocess
import sys
from pathlib import Path

GAP = 0.4
TS = re.compile(r"(\d+):(\d\d):(\d\d)\.(\d{3}) --> (\d+):(\d\d):(\d\d)\.(\d{3})")


def secs(h, m, s, ms):
    return int(h) * 3600 + int(m) * 60 + int(s) + int(ms) / 1000


def fmt(t):
    t = max(0.0, t)
    h = int(t // 3600)
    m = int(t % 3600 // 60)
    s = t % 60
    return f"{h:02d}:{m:02d}:{s:06.3f}"


def parse(vtt):
    lines = Path(vtt).read_text(encoding="utf-8").splitlines()
    cues, i = [], 0
    while i < len(lines):
        m = TS.search(lines[i])
        if m:
            g = m.groups()
            a, b = secs(*g[:4]), secs(*g[4:])
            ident = lines[i - 1] if i > 0 and lines[i - 1].strip() else ""
            j = i + 1
            text = []
            while j < len(lines) and lines[j].strip():
                text.append(lines[j])
                j += 1
            cues.append((a, b, ident, text))
            i = j
        else:
            i += 1
    return cues


def snap_start(cues, target):
    for k, (a, _, _, _) in enumerate(cues):
        if a < target:
            continue
        busy = max([c[1] for c in cues[:k]] or [0.0])
        if busy <= a - GAP:
            return a - GAP / 2
    raise SystemExit("no start gap found")


def snap_end(cues, target, start):
    best = None
    for k, (_, b, _, _) in enumerate(cues):
        if b > target or cues[k][0] < start:
            continue
        nxt = min([c[0] for c in cues[k + 1:]] or [b + 10])
        if nxt >= b + GAP:
            best = b + GAP / 2
    if best is None:
        raise SystemExit("no end gap found")
    return best


def main():
    wav, vtt, out, start, length = sys.argv[1:6]
    cues = parse(vtt)
    s = snap_start(cues, float(start))
    e = snap_end(cues, float(start) + float(length), s)
    keep = [c for c in cues if c[0] >= s and c[1] <= e]
    outd = Path(out)
    outd.mkdir(parents=True, exist_ok=True)
    body = ["WEBVTT", ""]
    for a, b, ident, text in keep:
        if ident:
            body.append(ident)
        body.append(f"{fmt(a - s)} --> {fmt(b - s)}")
        body.extend(text)
        body.append("")
    (outd / "reference.vtt").write_text("\n".join(body), encoding="utf-8")
    # the window in source-recording seconds, to crop other transcripts of
    # the same recording to the same span
    (outd / "window.json").write_text(
        json.dumps({"start_s": s, "end_s": e}), encoding="utf-8"
    )
    subprocess.run(
        ["ffmpeg", "-nostdin", "-loglevel", "error", "-y", "-i", wav, "-ss", f"{s:.3f}",
         "-t", f"{e - s:.3f}", "-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le",
         str(outd / "recording.mixed.wav")],
        check=True,
    )
    print(f"{out}: window {s:.2f}-{e:.2f} s ({(e - s) / 60:.2f} min), {len(keep)} cues")


if __name__ == "__main__":
    main()
