//! fly-core: UI-agnostic, OS-agnostic domain model for Fly on the Wall.
//!
//! Notes, folders, meetings, transcripts, templates, provenance, and the
//! word↔speaker aligner live here. Nothing in this crate may depend on an
//! operating system API, a UI framework, or a network client.

pub mod align;
pub mod crosstalk;
pub mod enhance;
pub mod model;
pub mod prompt_profile;
pub mod rediarize;
pub mod repeat;
pub mod seams;

pub use align::{align_words_to_speakers, AlignOptions};
pub use model::*;

/// Generate a new unique id (UUID v4, lowercase hyphenated).
pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}
