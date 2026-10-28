//! `hashed-bow-v1`: the deterministic, model-free vector channel used in CI.
//!
//! Lowercase alphanumeric words of 3+ characters, minus stopwords, hashed
//! with 32-bit FNV-1a into 512 buckets, counted, then L2-normalized. It
//! knows nothing about meaning, so it is a floor for what a real embedder
//! does in the app (README, "Retrieval").

use fly_storage::Storage;

pub const MODEL: &str = "hashed-bow-v1";
pub const DIMS: usize = 512;

const STOPWORDS: &[&str] = &[
    "the", "and", "for", "that", "with", "this", "have", "has", "had", "from", "what", "when",
    "where", "who", "whom", "which", "why", "how", "our", "are", "was", "were", "will", "would",
    "should", "could", "did", "does", "doing", "done", "you", "your", "yours", "they", "them",
    "their", "there", "here", "about", "into", "onto", "just", "then", "than", "all", "any", "can",
    "not", "but", "its", "it's", "let", "let's", "lets", "get", "got", "out", "yes", "okay",
    "also", "too", "very", "some", "each", "been", "being", "these", "those", "she", "her", "his",
    "him", "now", "still", "say", "said", "says", "way", "much", "many", "more", "most", "own",
    "same", "such", "only", "over", "under", "again", "once", "off", "per", "we're", "i'm", "i'll",
    "i'd", "you'd", "we'd", "don't", "didn't", "isn't", "won't", "that's", "there's", "what's",
    "it'll",
];

fn fnv1a(s: &str) -> u32 {
    s.bytes().fold(0x811c_9dc5u32, |h, b| {
        (h ^ b as u32).wrapping_mul(0x0100_0193)
    })
}

/// The words the embedder counts.
pub fn tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .map(|w| w.trim_matches('\'').to_lowercase())
        .filter(|w| w.chars().count() >= 3 && !STOPWORDS.contains(&w.as_str()))
        .collect()
}

pub fn embed(text: &str) -> Vec<f32> {
    let mut v = vec![0f32; DIMS];
    for t in tokens(text) {
        v[fnv1a(&t) as usize % DIMS] += 1.0;
    }
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut v {
            *x /= norm;
        }
    }
    v
}

/// Embed every pending chunk (title + text, as the app's document prompt
/// folds the title in).
pub fn embed_pending(storage: &Storage) -> anyhow::Result<usize> {
    let mut n = 0;
    loop {
        let batch = storage.pending_embedding_chunks(MODEL, 256)?;
        if batch.is_empty() {
            return Ok(n);
        }
        let rows: Vec<(i64, Vec<f32>)> = batch
            .iter()
            .map(|c| (c.id, embed(&format!("{}\n{}", c.title, c.text))))
            .collect();
        n += rows.len();
        storage.store_chunk_embeddings(MODEL, &rows)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_normalized() {
        let a = embed("The launch budget is ninety two hundred dollars");
        assert_eq!(a, embed("the LAUNCH budget is ninety two hundred dollars!"));
        let norm: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5);
        assert_eq!(embed("the and of"), vec![0f32; DIMS]);
        assert_eq!(
            tokens("What's the Q3 budget?"),
            vec!["budget".to_string()],
            "short words and stopwords drop"
        );
        // The hash is pinned: changing it changes every CI number.
        assert_eq!(fnv1a("budget"), 0x39ba_a088);
    }
}
