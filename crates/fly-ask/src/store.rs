//! How the harness reaches storage without holding it across a model call.
//!
//! The app keeps its `Storage` behind a `std::sync::Mutex` (rusqlite
//! connections aren't `Sync`). An Ask runs for seconds to minutes, so the
//! harness only ever takes the lock for one synchronous read at a time and
//! never across an `.await`. The evals hand in a plain `&Storage`.
//!
//! The harness is generic over [`StoreAccess`], so its future is `Send`
//! exactly when the store reference is (`&Mutex<Storage>` yes, `&Storage`
//! no — the evals run on a current-thread runtime and don't need it).

use fly_storage::Storage;

/// One synchronous read (or a few) against storage.
pub trait StoreAccess {
    fn with<R>(&self, f: impl FnOnce(&Storage) -> R) -> R;
}

impl StoreAccess for Storage {
    fn with<R>(&self, f: impl FnOnce(&Storage) -> R) -> R {
        f(self)
    }
}

impl StoreAccess for std::sync::Mutex<Storage> {
    fn with<R>(&self, f: impl FnOnce(&Storage) -> R) -> R {
        // A poisoned lock means another command panicked mid-write; reads
        // still see a consistent SQLite database.
        let guard = self.lock().unwrap_or_else(|p| p.into_inner());
        f(&guard)
    }
}
