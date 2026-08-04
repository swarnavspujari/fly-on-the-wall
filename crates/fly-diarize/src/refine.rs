//! VBx refinement of an initial (deliberately over-split) clustering.
//!
//! The sherpa sidecar stays the segmentation + initial-clustering provider;
//! this module cuts its turns into 1.44 s / 0.72 s subsegments, embeds each
//! with the SAME pinned speaker-embedding ONNX file the sidecar uses,
//! whitens the embeddings with a two-covariance model estimated from the
//! initial clusters themselves (no externally trained PLDA exists for these
//! embeddings — measured to work on the committed fixtures), and lets VBx
//! merge shattered clusters and re-attribute their time. This is what
//! removed the phantom-speaker clusters that survived every agglomerative
//! threshold (docs/BENCHMARKS.md, Phase 3, 2026-08-04).

use std::path::Path;

use fly_core::SpeakerTurn;

use crate::embed::EmbeddingExtractor;
use crate::vbx::{vbx, VbxParams};

/// 1.44 s windows, 0.72 s hop — the standard VBx subsegmentation.
const WIN_MS: u64 = 1_440;
const HOP_MS: u64 = 720;
/// Spans shorter than this cannot produce a stable embedding.
const MIN_MS: u64 = 300;

/// Subsegment cuts: (start_ms, end_ms, init_label), time-ordered.
fn cut_subsegments(init_turns: &[SpeakerTurn]) -> (Vec<(u64, u64, usize)>, Vec<String>) {
    let mut keys: Vec<String> = Vec::new();
    let mut cuts: Vec<(u64, u64, usize)> = Vec::new();
    for t in init_turns {
        let label = match keys.iter().position(|k| *k == t.speaker_key) {
            Some(i) => i,
            None => {
                keys.push(t.speaker_key.clone());
                keys.len() - 1
            }
        };
        let (s, e) = (t.start_ms, t.end_ms);
        if e.saturating_sub(s) < MIN_MS {
            continue;
        }
        if e - s <= WIN_MS {
            cuts.push((s, e, label));
            continue;
        }
        let mut at = s;
        while at + WIN_MS <= e {
            cuts.push((at, at + WIN_MS, label));
            at += HOP_MS;
        }
        if at < e && e - at >= MIN_MS {
            // tail window anchored to the turn end
            cuts.push((e - WIN_MS.min(e - s), e, label));
        }
    }
    cuts.sort_unstable();
    (cuts, keys)
}

/// Merge adjacent same-label subsegments into turns; where different labels
/// overlap (the 0.72 s hop), the boundary lands at the overlap midpoint —
/// the same policy as BUT's merge_adjacent_labels.
fn labels_to_turns(subsegs: &[(u64, u64, usize)], labels: &[usize]) -> Vec<SpeakerTurn> {
    // dense relabel by first appearance so keys come out spk_0, spk_1, …
    let mut order: Vec<usize> = Vec::new();
    let dense: Vec<usize> = labels
        .iter()
        .map(|l| match order.iter().position(|o| o == l) {
            Some(i) => i,
            None => {
                order.push(*l);
                order.len() - 1
            }
        })
        .collect();
    let mut turns: Vec<SpeakerTurn> = Vec::new();
    for (i, &(s, e, _)) in subsegs.iter().enumerate() {
        let key = format!("spk_{}", dense[i]);
        match turns.last_mut() {
            Some(last) if last.speaker_key == key && s <= last.end_ms => {
                last.end_ms = last.end_ms.max(e);
            }
            Some(last) if s < last.end_ms => {
                let mid = (s + last.end_ms) / 2;
                last.end_ms = mid;
                turns.push(SpeakerTurn {
                    speaker_key: key,
                    start_ms: mid,
                    end_ms: e,
                });
            }
            _ => turns.push(SpeakerTurn {
                speaker_key: key,
                start_ms: s,
                end_ms: e,
            }),
        }
    }
    turns
}

/// Subsegment embeddings + the whitened scoring space, prepared once so
/// parameter sweeps (the harness grid) can re-run VBx cheaply.
pub struct VbxLab {
    subsegs: Vec<(u64, u64, usize)>,
    /// whitened embeddings, row-major T×D
    x: Vec<f64>,
    d: usize,
    n_init: usize,
    phi: Vec<f64>,
    pub n_subsegs: usize,
    pub embed_secs: f64,
}

impl VbxLab {
    /// Cut init turns into subsegments, embed each, estimate the
    /// two-covariance whitening from the init clustering.
    pub fn prepare(
        samples_16k: &[f32],
        init_turns: &[SpeakerTurn],
        embedding_model: &Path,
        threads: usize,
    ) -> Result<Self, String> {
        let (cuts, _keys) = cut_subsegments(init_turns);
        if cuts.is_empty() {
            return Err("no subsegments to embed (empty or too-short turns)".into());
        }

        let started = std::time::Instant::now();
        let mut extractor = EmbeddingExtractor::new(embedding_model, threads)?;
        let mut subsegs = Vec::with_capacity(cuts.len());
        let mut embs: Vec<Vec<f32>> = Vec::with_capacity(cuts.len());
        for (s, e, label) in cuts {
            let (a, b) = (
                (s as usize * 16).min(samples_16k.len()),
                (e as usize * 16).min(samples_16k.len()),
            );
            if b <= a {
                continue;
            }
            if let Some(emb) = extractor.embed(&samples_16k[a..b])? {
                subsegs.push((s, e, label));
                embs.push(emb);
            }
        }
        let embed_secs = started.elapsed().as_secs_f64();
        if embs.is_empty() {
            return Err("no subsegment produced an embedding".into());
        }
        let n_init = subsegs.iter().map(|c| c.2).max().unwrap_or(0) + 1;

        // --- two-covariance whitening estimated from the init clusters ---
        let (t_n, d) = (embs.len(), embs[0].len());
        let mut x: Vec<f64> = Vec::with_capacity(t_n * d);
        for e in &embs {
            x.extend(e.iter().map(|v| *v as f64));
        }
        // global mean out
        let mut mean = vec![0.0f64; d];
        for i in 0..t_n {
            for k in 0..d {
                mean[k] += x[i * d + k];
            }
        }
        for m in mean.iter_mut() {
            *m /= t_n as f64;
        }
        for i in 0..t_n {
            for k in 0..d {
                x[i * d + k] -= mean[k];
            }
        }
        // per-dim within-class variance over init clusters
        let mut cluster_mean = vec![0.0f64; n_init * d];
        let mut cluster_n = vec![0usize; n_init];
        for (i, (_, _, l)) in subsegs.iter().enumerate() {
            cluster_n[*l] += 1;
            for k in 0..d {
                cluster_mean[l * d + k] += x[i * d + k];
            }
        }
        for l in 0..n_init {
            for k in 0..d {
                cluster_mean[l * d + k] /= cluster_n[l].max(1) as f64;
            }
        }
        let mut within = vec![0.0f64; d];
        for (i, (_, _, l)) in subsegs.iter().enumerate() {
            for k in 0..d {
                let dv = x[i * d + k] - cluster_mean[l * d + k];
                within[k] += dv * dv;
            }
        }
        for w in within.iter_mut() {
            *w = (*w / t_n as f64).max(1e-8);
        }
        // whiten to unit within-class variance
        for i in 0..t_n {
            for k in 0..d {
                x[i * d + k] /= within[k].sqrt();
            }
        }
        // across-class variance (of cluster means) in the whitened space
        let mut phi = vec![0.0f64; d];
        for l in 0..n_init {
            let w = cluster_n[l] as f64 / t_n as f64;
            for k in 0..d {
                let m = cluster_mean[l * d + k] / within[k].sqrt();
                phi[k] += w * m * m;
            }
        }
        for p in phi.iter_mut() {
            *p = p.max(1e-3);
        }

        Ok(Self {
            n_subsegs: subsegs.len(),
            subsegs,
            x,
            d,
            n_init,
            phi,
            embed_secs,
        })
    }

    /// One VBx pass → refined speaker turns (+ iteration count).
    pub fn run(&self, params: &VbxParams) -> (Vec<SpeakerTurn>, usize) {
        let init_labels: Vec<usize> = self.subsegs.iter().map(|c| c.2).collect();
        let out = vbx(
            &self.x,
            self.subsegs.len(),
            self.d,
            &self.phi,
            &init_labels,
            self.n_init,
            params,
        );
        (labels_to_turns(&self.subsegs, &out.labels), out.elbo.len())
    }
}

/// The full refinement step the engine runs: prepare + one pass with the
/// shipped parameters.
pub fn refine_with_vbx(
    samples_16k: &[f32],
    init_turns: &[SpeakerTurn],
    embedding_model: &Path,
    threads: usize,
    params: &VbxParams,
) -> Result<Vec<SpeakerTurn>, String> {
    let lab = VbxLab::prepare(samples_16k, init_turns, embedding_model, threads)?;
    let (turns, iters) = lab.run(params);
    tracing::info!(
        subsegments = lab.n_subsegs,
        embed_secs = format!("{:.1}", lab.embed_secs).as_str(),
        vbx_iters = iters,
        init_clusters = init_turns
            .iter()
            .map(|t| t.speaker_key.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        final_clusters = turns
            .iter()
            .map(|t| t.speaker_key.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        "vbx refinement done"
    );
    Ok(turns)
}

/// Read a wav as 16 kHz mono f32 — the pipeline always hands the diarizer
/// its own 16 kHz mono intermediates; other rates are linearly resampled,
/// multi-channel is averaged.
pub fn read_wav_mono_16k(path: &Path) -> Result<Vec<f32>, String> {
    let mut reader = hound::WavReader::open(path).map_err(|e| e.to_string())?;
    let spec = reader.spec();
    let channels = spec.channels.max(1) as usize;
    let mono: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1i64 << (spec.bits_per_sample - 1)) as f32;
            let samples: Vec<f32> = reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 * scale))
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?;
            samples
        }
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?,
    };
    let mono: Vec<f32> = if channels == 1 {
        mono
    } else {
        mono.chunks(channels)
            .map(|c| c.iter().sum::<f32>() / channels as f32)
            .collect()
    };
    if spec.sample_rate == 16_000 {
        return Ok(mono);
    }
    // linear resample to 16 kHz
    let ratio = spec.sample_rate as f64 / 16_000.0;
    let out_len = (mono.len() as f64 / ratio) as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let pos = i as f64 * ratio;
        let a = pos as usize;
        let frac = (pos - a as f64) as f32;
        let s0 = mono.get(a).copied().unwrap_or(0.0);
        let s1 = mono.get(a + 1).copied().unwrap_or(s0);
        out.push(s0 + (s1 - s0) * frac);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(key: &str, start_ms: u64, end_ms: u64) -> SpeakerTurn {
        SpeakerTurn {
            speaker_key: key.into(),
            start_ms,
            end_ms,
        }
    }

    #[test]
    fn subsegments_cover_turns_with_hop() {
        let turns = [turn("spk_0", 0, 3_600), turn("spk_1", 4_000, 4_500)];
        let (cuts, keys) = cut_subsegments(&turns);
        assert_eq!(keys, vec!["spk_0", "spk_1"]);
        // 3.6 s turn: windows at 0, 720, 1440, 2160 (+ tail 2160..3600)
        assert!(cuts.iter().all(|&(s, e, _)| e > s));
        assert_eq!(cuts.first().unwrap().0, 0);
        assert_eq!(cuts.iter().filter(|c| c.2 == 1).count(), 1); // short turn = one cut
        let tail = cuts.iter().rfind(|c| c.2 == 0).unwrap();
        assert_eq!(tail.1, 3_600);
    }

    #[test]
    fn tiny_turns_are_skipped() {
        let (cuts, _) = cut_subsegments(&[turn("spk_0", 0, 200)]);
        assert!(cuts.is_empty());
    }

    #[test]
    fn labels_merge_and_split_at_overlap_midpoint() {
        // two overlapping subsegments with different labels: boundary at the
        // midpoint of the overlap; same-label neighbors merge
        let subsegs = vec![(0u64, 1_440u64, 0usize), (720, 2_160, 0), (1_440, 2_880, 1)];
        let turns = labels_to_turns(&subsegs, &[0, 0, 1]);
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].speaker_key, "spk_0");
        assert_eq!(turns[0].start_ms, 0);
        assert_eq!(turns[0].end_ms, 1_800); // (1440+2160)/2
        assert_eq!(turns[1].speaker_key, "spk_1");
        assert_eq!(turns[1].start_ms, 1_800);
        assert_eq!(turns[1].end_ms, 2_880);
    }

    #[test]
    fn dense_relabel_orders_by_first_appearance() {
        let subsegs = vec![(0u64, 500u64, 7usize), (600, 1_100, 2), (1_200, 1_700, 7)];
        let turns = labels_to_turns(&subsegs, &[7, 2, 7]);
        assert_eq!(turns[0].speaker_key, "spk_0");
        assert_eq!(turns[1].speaker_key, "spk_1");
        assert_eq!(turns[2].speaker_key, "spk_0");
    }
}
