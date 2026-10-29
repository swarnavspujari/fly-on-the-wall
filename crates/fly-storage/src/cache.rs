//! In-memory caches of derived search data, valid while the database is
//! unchanged.
//!
//! * **Vectors.** A semantic search scores every embedded chunk. Reading
//!   each vector out of SQLite for every query cost seconds at a thousand
//!   meetings (35k chunks; the 3 KB vectors sit in overflow pages, so even
//!   reading a row's `model` column walks them). The vectors are kept in
//!   memory between searches, quantized to `i8` with a per-vector scale (a
//!   quarter of the f32 size, about 27 MB at that library size), with each
//!   chunk's owner. Ranking runs on the quantized copy; the winners are
//!   re-scored from the stored f32 vectors.
//! * **Segment ranges.** Keyword statistics map each matching FTS row to
//!   its meeting. `transcript_segments` rows of one meeting are written by
//!   one trigger statement, so they hold one contiguous rowid range; the
//!   ranges (and per-meeting segment counts) let a rowid be mapped without
//!   reading its text-heavy row. If ranges ever overlap, the caller falls
//!   back to joining.
//!
//! **Validity.** A stamp of `PRAGMA data_version` (which changes when ANY
//! other connection commits) and this connection's `total_changes()` (every
//! row it inserts, updates or deletes, trigger programs included) says
//! "nothing changed since the cache was built". Any write anywhere drops
//! the caches, so they can never hold a vector or range a write replaced
//! or removed. Visibility (folder, dates, keep-local) is never cached:
//! every search asks the database which owners it may see.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::{Result, Storage};

#[derive(Default)]
pub(crate) struct ReadCache {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    stamp: Option<(i64, u64)>,
    vectors: Option<Arc<VectorMatrix>>,
    ranges: Option<Arc<Option<SegmentRanges>>>,
}

/// Which table a chunk's owner lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum OwnerKind {
    Note,
    Transcript,
    /// Any other `owner_kind` (none is written today): no folder, date or
    /// privacy can be checked for it.
    Other,
}

/// Every embedded chunk of one model and dimension, quantized.
pub(crate) struct VectorMatrix {
    model: String,
    dims: usize,
    pub(crate) ids: Vec<i64>,
    /// Per chunk: index into `owners`.
    pub(crate) owner_of: Vec<u32>,
    /// Distinct owners (kind, note id / meeting id).
    pub(crate) owners: Vec<(OwnerKind, String)>,
    scales: Vec<f32>,
    data: Vec<i8>,
}

impl VectorMatrix {
    /// Approximate dot product of row `i` with `query` (|error| ≈ 1e-3 for
    /// unit vectors: far below what ranking or a similarity floor resolves).
    pub(crate) fn score(&self, i: usize, query: &[f32]) -> f32 {
        let row = &self.data[i * self.dims..(i + 1) * self.dims];
        let dot: f32 = row.iter().zip(query).map(|(q, x)| *q as f32 * x).sum();
        dot * self.scales[i]
    }

    pub(crate) fn len(&self) -> usize {
        self.ids.len()
    }
}

/// Symmetric per-vector quantization: the largest component maps to ±127.
fn quantize(v: impl Iterator<Item = f32> + Clone, out: &mut Vec<i8>) -> f32 {
    let max = v.clone().fold(0f32, |m, x| m.max(x.abs()));
    if max == 0.0 {
        out.extend(v.map(|_| 0i8));
        return 0.0;
    }
    let scale = max / 127.0;
    out.extend(v.map(|x| (x / scale).round().clamp(-127.0, 127.0) as i8));
    scale
}

/// Per meeting, the rowid range of its `transcript_segments` rows.
pub(crate) struct SegmentRanges {
    /// (first rowid, last rowid, meeting index), sorted, non-overlapping.
    spans: Vec<(i64, i64, u32)>,
    pub(crate) meetings: Vec<String>,
    /// Segment rows per meeting (same index as `meetings`).
    pub(crate) counts: Vec<usize>,
}

impl SegmentRanges {
    /// The meeting (index into `meetings`) whose range holds `rowid`.
    pub(crate) fn meeting_of(&self, rowid: i64) -> Option<u32> {
        let i = self.spans.partition_point(|(start, _, _)| *start <= rowid);
        let (start, end, m) = *self.spans.get(i.checked_sub(1)?)?;
        (start <= rowid && rowid <= end).then_some(m)
    }
}

impl Storage {
    fn read_stamp(&self) -> Result<(i64, u64)> {
        let data_version: i64 = self
            .conn
            .pragma_query_value(None, "data_version", |r| r.get(0))?;
        Ok((data_version, self.conn.total_changes()))
    }

    /// The cache, emptied first if anything changed since it was filled.
    fn fresh_cache(&self) -> Result<std::sync::MutexGuard<'_, Inner>> {
        let stamp = self.read_stamp()?;
        let mut inner = self
            .read_cache
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if inner.stamp != Some(stamp) {
            *inner = Inner {
                stamp: Some(stamp),
                ..Default::default()
            };
        }
        Ok(inner)
    }

    /// The embedded chunks of `model` whose vectors have `dims` components,
    /// from the cache when nothing changed since it was built.
    pub(crate) fn vector_matrix(&self, model: &str, dims: usize) -> Result<Arc<VectorMatrix>> {
        let mut inner = self.fresh_cache()?;
        if let Some(m) = &inner.vectors {
            if m.model == model && m.dims == dims {
                return Ok(m.clone());
            }
        }
        let built = Arc::new(self.load_vector_matrix(model, dims)?);
        inner.vectors = Some(built.clone());
        Ok(built)
    }

    fn load_vector_matrix(&self, model: &str, dims: usize) -> Result<VectorMatrix> {
        let mut m = VectorMatrix {
            model: model.to_string(),
            dims,
            ids: Vec::new(),
            owner_of: Vec::new(),
            owners: Vec::new(),
            scales: Vec::new(),
            data: Vec::new(),
        };
        let mut owner_index: HashMap<(OwnerKind, String), u32> = HashMap::new();
        let mut stmt = self.conn.prepare(
            "SELECT id, owner_kind, owner_id, embedding FROM embedding_chunks
              WHERE embedding IS NOT NULL AND model = ?1",
        )?;
        let mut rows = stmt.query([model])?;
        while let Some(r) = rows.next()? {
            let kind = match r.get_ref(1)?.as_str().unwrap_or("") {
                "note" => OwnerKind::Note,
                "transcript" => OwnerKind::Transcript,
                _ => OwnerKind::Other,
            };
            let rusqlite::types::ValueRef::Blob(blob) = r.get_ref(3)? else {
                continue;
            };
            if blob.len() != dims * 4 {
                continue; // another dimension: not comparable with this query
            }
            let owner: String = r.get(2)?;
            let next = m.owners.len() as u32;
            let idx = *owner_index.entry((kind, owner.clone())).or_insert_with(|| {
                m.owners.push((kind, owner));
                next
            });
            let (floats, _) = blob.as_chunks::<4>();
            m.ids.push(r.get(0)?);
            m.owner_of.push(idx);
            let scale = quantize(floats.iter().map(|b| f32::from_le_bytes(*b)), &mut m.data);
            m.scales.push(scale);
        }
        Ok(m)
    }

    /// Per-meeting rowid ranges of `transcript_segments`, or `None` when
    /// two meetings' ranges overlap (then rowids can't be mapped this way).
    pub(crate) fn segment_ranges(&self) -> Result<Arc<Option<SegmentRanges>>> {
        let mut inner = self.fresh_cache()?;
        if let Some(r) = &inner.ranges {
            return Ok(r.clone());
        }
        let built = Arc::new(self.load_segment_ranges()?);
        inner.ranges = Some(built.clone());
        Ok(built)
    }

    fn load_segment_ranges(&self) -> Result<Option<SegmentRanges>> {
        let mut r = SegmentRanges {
            spans: Vec::new(),
            meetings: Vec::new(),
            counts: Vec::new(),
        };
        let mut stmt = self.conn.prepare(
            "SELECT meeting_id, min(id), max(id), count(*) FROM transcript_segments
              WHERE meeting_id IS NOT NULL GROUP BY meeting_id",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let i = r.meetings.len() as u32;
            r.meetings.push(row.get(0)?);
            r.spans.push((row.get(1)?, row.get(2)?, i));
            r.counts.push(row.get::<_, i64>(3)?.max(0) as usize);
        }
        r.spans.sort_unstable();
        if r.spans.windows(2).any(|w| w[0].1 >= w[1].0) {
            return Ok(None);
        }
        Ok(Some(r))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_storage;

    #[test]
    fn quantized_scores_track_exact_cosine() {
        let mut data = Vec::new();
        let v: Vec<f32> = (0..768)
            .map(|i| ((i * 37 % 101) as f32 - 50.0) / 50.0)
            .collect();
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        let unit: Vec<f32> = v.iter().map(|x| x / norm).collect();
        let scale = quantize(unit.iter().copied(), &mut data);
        let m = VectorMatrix {
            model: "m".into(),
            dims: 768,
            ids: vec![1],
            owner_of: vec![0],
            owners: vec![(OwnerKind::Note, "n".into())],
            scales: vec![scale],
            data,
        };
        let q: Vec<f32> = (0..768)
            .map(|i| ((i * 13 % 7) as f32 - 3.0) / 3.0)
            .collect();
        let qn = q.iter().map(|x| x * x).sum::<f32>().sqrt();
        let q: Vec<f32> = q.iter().map(|x| x / qn).collect();
        let exact: f32 = unit.iter().zip(&q).map(|(a, b)| a * b).sum();
        assert!((m.score(0, &q) - exact).abs() < 2e-3);
        assert!((m.score(0, &unit) - 1.0).abs() < 2e-3);
    }

    #[test]
    fn any_write_drops_the_cache() {
        let (_dir, s) = test_storage();
        let note = s.create_note("n", None).unwrap();
        s.update_note_scratchpad(&note.id, "first body").unwrap();
        let p = s.pending_embedding_chunks("m", 10).unwrap();
        s.store_chunk_embeddings("m", &[(p[0].id, vec![1.0, 0.0])])
            .unwrap();
        let a = s.vector_matrix("m", 2).unwrap();
        assert_eq!(a.len(), 1);
        let b = s.vector_matrix("m", 2).unwrap();
        assert!(Arc::ptr_eq(&a, &b), "unchanged database: same cache");
        // A write on this connection.
        s.update_note_scratchpad(&note.id, "second body").unwrap();
        let c = s.vector_matrix("m", 2).unwrap();
        assert!(!Arc::ptr_eq(&a, &c));
        assert_eq!(c.len(), 0, "the edited chunk is pending again");
        // A write from another connection.
        let other = Storage::open(s.data_dir()).unwrap();
        let p = other.pending_embedding_chunks("m", 10).unwrap();
        other
            .store_chunk_embeddings("m", &[(p[0].id, vec![0.0, 1.0])])
            .unwrap();
        let d = s.vector_matrix("m", 2).unwrap();
        assert_eq!(d.len(), 1, "another connection's commit is seen");
        // Other models and dimensions are separate.
        assert_eq!(s.vector_matrix("m", 3).unwrap().len(), 0);
        assert_eq!(s.vector_matrix("other", 2).unwrap().len(), 0);
    }
}
