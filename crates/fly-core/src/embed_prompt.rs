//! The embedding model and the prompts it's fed, shared by the app's
//! embedding pipeline (`src-tauri/src/embeddings.rs`) and the retrieval eval
//! (`evals/runner`), so the eval measures exactly what the app indexes.

/// Embedding model tag pulled into the managed Ollama runtime.
pub const EMBED_MODEL: &str = "embeddinggemma:300m";

/// EmbeddingGemma's document prompt. The owning note/meeting title rides in
/// the `title:` slot, so "the airline deal meeting" can match a chunk whose
/// text never says "airline".
pub fn doc_prompt(title: &str, text: &str) -> String {
    let title = title.trim();
    let title = if title.is_empty() { "none" } else { title };
    format!("title: {title} | text: {text}")
}

/// EmbeddingGemma's retrieval query prompt (asymmetric to `doc_prompt`).
pub fn query_prompt(query: &str) -> String {
    format!("task: search result | query: {query}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompts_are_pinned() {
        assert_eq!(doc_prompt(" ", "x"), "title: none | text: x");
        assert_eq!(doc_prompt("Sync", "x"), "title: Sync | text: x");
        assert_eq!(query_prompt("q"), "task: search result | query: q");
    }
}
