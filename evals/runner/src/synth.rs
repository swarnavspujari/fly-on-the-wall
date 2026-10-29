//! A seeded synthetic library for the scale benchmark (local_bench): the
//! fixture meetings plus N generated meetings stitched from shareable
//! fixture lines, with 768-dimension vectors (the real embedder's size).
//! The text is synthetic, so the vectors are random: they make the vector
//! scan cost what it costs in the app, not rank anything meaningfully.
//! Same seed, same library.

use anyhow::Result;
use chrono::{Duration, TimeZone, Utc};
use fly_storage::Storage;

use crate::corpus::{AttendeeFixture, Corpus, MeetingFixture};

/// The real embedder's dimensions (embeddinggemma:300m).
pub const DIMS: usize = 768;
/// The model id stored on the synthetic vectors.
pub const MODEL: &str = "synthetic-768";
/// Lines per generated meeting (~45-minute meeting at fixture line length).
pub const SEGMENTS_PER_MEETING: usize = 60;

/// xorshift64*: small, seeded, the same everywhere.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }

    /// Uniform in [-1, 1).
    pub fn signed(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 23) as f32 - 1.0
    }
}

const TOPICS: &[&str] = &[
    "Weekly sync",
    "Customer call",
    "Planning",
    "Design review",
    "Support review",
    "Sales pipeline",
    "Hiring loop",
    "Ops check-in",
];

/// The fixtures plus `n` generated meetings (`s0000`…).
pub fn library(fixtures: &Corpus, n: usize, seed: u64) -> Result<Corpus> {
    let pool: Vec<(bool, String)> = fixtures
        .meetings
        .iter()
        .filter(|m| !fixtures.is_keep_local(m) && !m.red_team)
        .flat_map(|m| m.segments.iter().map(|s| (s.1 == "mic", s.3.clone())))
        .collect();
    let folders: Vec<String> = fixtures
        .folder_paths()
        .into_iter()
        .filter(|f| !f.starts_with("People"))
        .collect();
    let mut rng = Rng::new(seed);
    let mut meetings = fixtures.meetings.clone();
    let start = Utc
        .with_ymd_and_hms(2024, 1, 8, 15, 0, 0)
        .single()
        .expect("valid");
    for i in 0..n {
        let id = format!("s{i:04}");
        let mut t = 4u64;
        let mut segments = Vec::with_capacity(SEGMENTS_PER_MEETING);
        for j in 0..SEGMENTS_PER_MEETING {
            let (mic, text) = &pool[rng.below(pool.len())];
            let key = if *mic {
                "mic".to_string()
            } else {
                format!("spk_{}", rng.below(3))
            };
            segments.push((
                format!("{id}-{:02}", j + 1),
                key,
                format!("{}:{:02}", t / 60, t % 60),
                text.clone(),
            ));
            t += 4 + (text.split_whitespace().count() as u64 * 2) / 5;
        }
        meetings.push(MeetingFixture {
            id: id.clone(),
            title: format!("{} {}", TOPICS[rng.below(TOPICS.len())], i + 1),
            started_at: start
                + Duration::days(rng.below(640) as i64)
                + Duration::minutes(i as i64 % 300),
            folder: (rng.below(3) > 0).then(|| folders[rng.below(folders.len())].clone()),
            keep_local: false,
            canaries: vec![],
            red_team: false,
            attendees: vec![AttendeeFixture {
                name: "Alex Placeholder".into(),
                email: None,
            }],
            speakers: [
                ("mic", "You"),
                ("spk_0", "Speaker 1"),
                ("spk_1", "Speaker 2"),
                ("spk_2", "Speaker 3"),
            ]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
            scratchpad: String::new(),
            segments,
            items: vec![],
            enhanced: vec![],
            cleaned: Default::default(),
            keep_local_folder: None,
        });
    }
    Corpus::from_meetings(meetings)
}

/// A unit vector derived from `seed` and `key`.
pub fn vector(seed: u64, key: u64) -> Vec<f32> {
    let mut rng = Rng::new(seed ^ key.wrapping_mul(0xD6E8_FEB8_6659_FD93));
    let mut v: Vec<f32> = (0..DIMS).map(|_| rng.signed()).collect();
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        v.iter_mut().for_each(|x| *x /= norm);
    }
    v
}

/// Store a synthetic vector for every pending chunk.
pub fn index(storage: &Storage, seed: u64) -> Result<usize> {
    let mut n = 0;
    loop {
        let batch = storage.pending_embedding_chunks(MODEL, 512)?;
        if batch.is_empty() {
            return Ok(n);
        }
        let rows: Vec<(i64, Vec<f32>)> = batch
            .iter()
            .map(|c| (c.id, vector(seed, c.id as u64)))
            .collect();
        n += rows.len();
        storage.store_chunk_embeddings(MODEL, &rows)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_library_is_seeded() {
        let fixtures = Corpus::load(&crate::evals_dir().join("fixtures")).unwrap();
        let a = library(&fixtures, 5, 42).unwrap();
        let b = library(&fixtures, 5, 42).unwrap();
        let c = library(&fixtures, 5, 43).unwrap();
        let text = |x: &Corpus| {
            x.meetings
                .iter()
                .map(|m| format!("{}{:?}{:?}", m.title, m.started_at, m.segments))
                .collect::<String>()
        };
        assert_eq!(text(&a), text(&b));
        assert_ne!(text(&a), text(&c));
        assert_eq!(a.meetings.len(), fixtures.meetings.len() + 5);
        assert_eq!(vector(1, 7), vector(1, 7));
        assert_eq!(vector(1, 7).len(), DIMS);
        let n: f32 = vector(1, 7).iter().map(|x| x * x).sum();
        assert!((n - 1.0).abs() < 1e-4);
    }
}
