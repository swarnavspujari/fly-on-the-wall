//! Bench-only engines served by a local HTTP runtime (see bench_engines.rs
//! for the shared pieces): NVIDIA NeMo-Speech.cpp `serve` and llama.cpp's
//! `llama-server` running IBM Granite Speech. One server per channel keeps
//! the model loaded across the shipped VAD batches — a fresh CLI process per
//! batch would pay model load + warmup every time (measured: 11.6 s warmup
//! for NeMo-Speech.cpp on CPU).

use std::path::{Path, PathBuf};

use fly_asr::{AsrError, RawTranscript, TranscribeOptions, TranscriptionEngine};
use fly_core::Word;

use crate::bench_align;
use crate::bench_engines::{
    pieces, record_transfer, reply_words, stat, SherpaEngine, TempWav, Worker,
};

pub struct Server {
    child: tokio::process::Child,
    pub base: String,
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .expect("a free localhost port")
}

impl Server {
    /// Start `exe args…` (`{port}` inside an arg is replaced) and wait until
    /// `ready_path` answers 2xx. The server's own log goes to a temp file.
    pub async fn start(
        exe: &Path,
        args: &[String],
        ready_path: &str,
        what: &str,
    ) -> Result<(Self, f64), AsrError> {
        let port = free_port();
        let started = std::time::Instant::now();
        let log = std::env::temp_dir().join(format!("fotw-bench-{what}-{port}.log"));
        let log_file = std::fs::File::create(&log).map_err(AsrError::Io)?;
        let mut cmd = tokio::process::Command::new(exe);
        cmd.args(args.iter().map(|a| a.replace("{port}", &port.to_string())))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(log_file)
            .kill_on_drop(true);
        if let Some(dir) = exe.parent().filter(|d| d.is_dir()) {
            // ggml loads backend DLLs from the CWD too (see parakeet.rs)
            cmd.current_dir(dir);
        }
        #[cfg(windows)]
        {
            // CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS, as the sidecars
            cmd.creation_flags(0x0800_0000 | 0x0000_4000);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| AsrError::Engine(format!("failed to launch {what}: {e}")))?;
        let base = format!("http://127.0.0.1:{port}");
        let client = reqwest::Client::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
        loop {
            if let Some(status) = child.try_wait().map_err(AsrError::Io)? {
                let tail = std::fs::read_to_string(&log).unwrap_or_default();
                let tail: Vec<&str> = tail.lines().rev().take(12).collect();
                return Err(AsrError::Engine(format!(
                    "{what} exited during startup ({status}): {}",
                    tail.into_iter().rev().collect::<Vec<_>>().join(" | ")
                )));
            }
            if let Ok(r) = client.get(format!("{base}{ready_path}")).send().await {
                if r.status().is_success() {
                    break;
                }
            }
            if std::time::Instant::now() > deadline {
                return Err(AsrError::Engine(format!(
                    "{what} not ready after 600 s (log: {})",
                    log.display()
                )));
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
        Ok((Server { child, base }, started.elapsed().as_secs_f64()))
    }

    pub async fn stop(mut self) {
        let _ = self.child.kill().await;
    }
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(1800))
        .build()
        .expect("http client")
}

/// Standard base64 (RFC 4648, padded) — the one place the harness needs it.
pub fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[derive(Clone, Copy, Debug)]
pub enum ServerFlavor {
    /// NeMo-Speech.cpp: POST /v1/audio/transcriptions, verbose_json words.
    NemoSpeech,
    /// llama.cpp + Granite Speech: POST /v1/chat/completions with the
    /// audio part BEFORE the prompt (the model's training order).
    GraniteChat,
}

/// IBM's model-card prompt for punctuated transcription.
const GRANITE_PROMPT: &str = "transcribe the speech with proper punctuation and capitalization.";

/// Batch engine over a local server: the shipped VAD batches, each cut into
/// ≤ `max_piece_ms` pieces at quiet points, POSTed one by one.
pub struct ServerEngine {
    pub label: &'static str,
    pub model_tag: String,
    pub exe: PathBuf,
    pub args: Vec<String>,
    pub ready_path: &'static str,
    pub max_piece_ms: Option<u64>,
    pub flavor: ServerFlavor,
    /// Timing anchor for decoders without timestamps (Granite).
    pub anchor: Option<Box<SherpaEngine>>,
}

fn secs_to_ms(s: f64) -> u64 {
    (s * 1000.0).round().max(0.0) as u64
}

impl ServerEngine {
    async fn post_piece(
        &self,
        server: &Server,
        anchor: Option<&mut Worker>,
        samples: &[f32],
        rate: u32,
    ) -> Result<Vec<Word>, AsrError> {
        let wav = TempWav::write(samples, rate)?;
        let bytes = std::fs::read(&wav.0).map_err(AsrError::Io)?;
        let secs = samples.len() as f64 / rate.max(1) as f64;
        let t0 = std::time::Instant::now();
        let client = http();
        let err = |e: String| AsrError::Engine(format!("{}: {e}", self.label));
        let req = match self.flavor {
            ServerFlavor::NemoSpeech => {
                let form = reqwest::multipart::Form::new()
                    .part(
                        "file",
                        reqwest::multipart::Part::bytes(bytes)
                            .file_name("piece.wav")
                            .mime_str("audio/wav")
                            .unwrap(),
                    )
                    .text("response_format", "verbose_json")
                    .text("language", "en");
                client
                    .post(format!("{}/v1/audio/transcriptions", server.base))
                    .multipart(form)
            }
            ServerFlavor::GraniteChat => client
                .post(format!("{}/v1/chat/completions", server.base))
                .json(&serde_json::json!({
                    "messages": [{
                        "role": "user",
                        "content": [
                            {"type": "input_audio",
                             "input_audio": {"data": base64(&bytes), "format": "wav"}},
                            {"type": "text", "text": GRANITE_PROMPT},
                        ],
                    }],
                    "temperature": 0.0,
                    // generous: ~3 words/s of speech, ~1.4 tokens per word
                    "max_tokens": (secs * 8.0) as u64 + 64,
                    "stream": false,
                })),
        };
        let r = req.send().await.map_err(|e| err(e.to_string()))?;
        let status = r.status();
        let v: serde_json::Value = r.json().await.map_err(|e| err(format!("bad json: {e}")))?;
        if !status.is_success() {
            return Err(err(format!("{status}: {v}")));
        }
        stat(
            &format!("{}_decode_secs", self.label),
            serde_json::json!(t0.elapsed().as_secs_f64()),
        );
        match self.flavor {
            ServerFlavor::NemoSpeech => Ok(v["words"]
                .as_array()
                .map(|ws| {
                    ws.iter()
                        .filter_map(|w| {
                            Some(Word {
                                text: w["word"].as_str()?.trim().to_string(),
                                start_ms: secs_to_ms(w["start"].as_f64()?),
                                end_ms: secs_to_ms(w["end"].as_f64()?),
                            })
                        })
                        .filter(|w| !w.text.is_empty())
                        .collect()
                })
                .unwrap_or_default()),
            ServerFlavor::GraniteChat => {
                if v["choices"][0]["finish_reason"].as_str() == Some("length") {
                    stat("granite_hit_max_tokens", serde_json::json!(1));
                }
                stat(
                    "granite_completion_tokens",
                    v["usage"]["completion_tokens"].clone(),
                );
                let text = v["choices"][0]["message"]["content"]
                    .as_str()
                    .unwrap_or_default()
                    // occasional markdown emphasis (model-card known issue)
                    .replace(['_', '*'], " ");
                let hyp = bench_align::split_text_words(&text);
                let anchors = match (anchor, &self.anchor) {
                    (Some(aw), Some(a)) => {
                        let arep = aw
                            .request(serde_json::json!({"wav": wav.0}), a.label)
                            .await?;
                        stat("anchor_decode_secs", arep["decode_secs"].clone());
                        reply_words(&arep).unwrap_or_default()
                    }
                    _ => Vec::new(),
                };
                let (words, st) =
                    bench_align::transfer_times(&hyp, &anchors, (0, secs_to_ms(secs)));
                record_transfer(st, self.anchor.as_ref().map(|a| a.label));
                Ok(words)
            }
        }
    }
}

#[async_trait::async_trait]
impl TranscriptionEngine for ServerEngine {
    fn id(&self) -> &'static str {
        self.label
    }

    fn is_local(&self) -> bool {
        true
    }

    fn model_tag(&self) -> Option<String> {
        Some(self.model_tag.clone())
    }

    async fn transcribe(
        &self,
        wav_path: &Path,
        opts: &TranscribeOptions,
    ) -> fly_asr::Result<RawTranscript> {
        self.transcribe_with_progress(wav_path, opts, &|_| {}, &|| false)
            .await
    }

    async fn transcribe_with_progress(
        &self,
        wav_path: &Path,
        opts: &TranscribeOptions,
        on_progress: fly_asr::TranscribeProgressFn<'_>,
        cancel: fly_asr::CancelFn<'_>,
    ) -> fly_asr::Result<RawTranscript> {
        let (server, load) =
            Server::start(&self.exe, &self.args, self.ready_path, self.label).await?;
        stat(
            &format!("{}_load_secs", self.label),
            serde_json::json!(load),
        );
        let anchor = match &self.anchor {
            Some(a) => Some(tokio::sync::Mutex::new(a.spawn_worker().await?)),
            None => None,
        };
        let server_ref = &server;
        let anchor_ref = &anchor;
        let raw = fly_asr::batch::transcribe_batched(
            self.label,
            &self.exe,
            wav_path,
            opts,
            false,
            Some(fly_asr::batch::MAX_BATCH_SPEECH_MS),
            on_progress,
            cancel,
            |chunk, rate| async move {
                let mut a = match anchor_ref {
                    Some(m) => Some(m.lock().await),
                    None => None,
                };
                let mut words = Vec::new();
                for (from, to) in pieces(&chunk, rate, self.max_piece_ms) {
                    let off = from as u64 * 1000 / rate as u64;
                    let got = self
                        .post_piece(server_ref, a.as_deref_mut(), &chunk[from..to], rate)
                        .await?;
                    words.extend(got.into_iter().map(|mut x| {
                        x.start_ms += off;
                        x.end_ms += off;
                        x
                    }));
                }
                Ok(RawTranscript {
                    language: Some("en".into()),
                    words,
                    segments: vec![],
                })
            },
        )
        .await;
        server.stop().await;
        if let Some(a) = anchor {
            a.into_inner().close().await;
        }
        let raw = raw?;
        Ok(RawTranscript {
            words: fly_asr::drop_non_speech_spans(raw.words),
            ..raw
        })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_matches_rfc4648_vectors() {
        for (i, o) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(super::base64(i.as_bytes()), o);
        }
    }
}
