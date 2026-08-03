//! In-process speaker-embedding extraction: 80-dim fbank + ONNX forward.
//!
//! Loads the same ONNX files the sherpa sidecar uses (CAM++ / ERes2NetV2 /
//! WeSpeaker exports, input `(1, T, 80)` float32 → output `(1, D)`), so the
//! pinned artifact story is unchanged. Feature normalization is the
//! per-segment "global-mean" the models declare in their metadata.

use std::path::Path;

use crate::fbank::{apply_cmn, FbankComputer, NUM_BINS};

pub struct EmbeddingExtractor {
    session: ort::session::Session,
    input_name: String,
    output_name: String,
    fbank: FbankComputer,
}

impl EmbeddingExtractor {
    pub fn new(model: &Path, threads: usize) -> Result<Self, String> {
        let session = ort::session::Session::builder()
            .map_err(|e| e.to_string())?
            .with_intra_threads(threads.max(1))
            .map_err(|e| e.to_string())?
            .commit_from_file(model)
            .map_err(|e| e.to_string())?;
        let input_name = session
            .inputs()
            .first()
            .ok_or("embedding model has no inputs")?
            .name()
            .to_string();
        let output_name = session
            .outputs()
            .first()
            .ok_or("embedding model has no outputs")?
            .name()
            .to_string();
        Ok(Self {
            session,
            input_name,
            output_name,
            fbank: FbankComputer::new(),
        })
    }

    /// Embedding for one span of 16 kHz mono samples; `None` when the span
    /// is too short to produce any frames. The result is L2-normalized.
    pub fn embed(&mut self, samples: &[f32]) -> Result<Option<Vec<f32>>, String> {
        let (mut feats, n_frames) = self.fbank.compute(samples);
        if n_frames < 5 {
            return Ok(None);
        }
        apply_cmn(&mut feats, n_frames);
        let tensor =
            ort::value::Tensor::from_array(([1usize, n_frames, NUM_BINS], feats))
                .map_err(|e| e.to_string())?;
        let outputs = self
            .session
            .run(ort::inputs![self.input_name.as_str() => tensor])
            .map_err(|e| e.to_string())?;
        let (_, data) = outputs[self.output_name.as_str()]
            .try_extract_tensor::<f32>()
            .map_err(|e| e.to_string())?;
        let mut emb: Vec<f32> = data.to_vec();
        if emb.iter().any(|v| !v.is_finite()) {
            return Ok(None);
        }
        let norm = emb.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-10);
        for v in emb.iter_mut() {
            *v /= norm;
        }
        Ok(Some(emb))
    }
}
