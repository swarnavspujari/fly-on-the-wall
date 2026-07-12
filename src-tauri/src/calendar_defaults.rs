//! Bundled default OAuth app credentials — injected at build time.
//!
//! These are *this app's own* OAuth client, compiled into release builds so
//! invited testers can connect their calendar in one click. They are the
//! fallback only — a user who pastes their own client ID / secret in
//! Settings › Technical (the BYO path) always overrides these (see
//! `calendar_commands::effective_*`).
//!
//! The values are NOT stored in the repo. They come from build-time
//! environment variables (`option_env!`), supplied by the release workflow
//! from GitHub Actions secrets — so a secret never lands in git history or the
//! public repo. A build with the vars unset compiles empty strings, leaving
//! both providers in the BYO-only state (the safe default).
//!
//! To bundle them:
//!   • Release (CI): set repo secrets `FOTW_GOOGLE_CLIENT_ID`,
//!     `FOTW_GOOGLE_CLIENT_SECRET`, `FOTW_MS_CLIENT_ID` — wired into the build
//!     env in `.github/workflows/release.yml`.
//!   • Local one-click build: export those three vars before
//!     `npm run tauri build` (or `dev`), then force a rebuild of this crate so
//!     `option_env!` re-reads them (`touch src-tauri/src/calendar_defaults.rs`,
//!     or `cargo clean -p looma-app`).
//!
//! Bundling the Google desktop-client secret is safe: for an installed app
//! using PKCE the "client secret" is non-confidential (Google's own docs say
//! so), and it grants nothing without the user completing interactive consent.
//! This is the ONE place the defaults live — nothing else hard-codes them.

/// Google OAuth client ID (`….apps.googleusercontent.com`).
pub const GOOGLE_CLIENT_ID: &str = match option_env!("FOTW_GOOGLE_CLIENT_ID") {
    Some(v) => v,
    None => "",
};

/// Google OAuth client secret (non-confidential under PKCE — see module docs).
pub const GOOGLE_CLIENT_SECRET: &str = match option_env!("FOTW_GOOGLE_CLIENT_SECRET") {
    Some(v) => v,
    None => "",
};

/// Azure application (client) ID. No secret — MS Graph uses a public PKCE client.
pub const MS_CLIENT_ID: &str = match option_env!("FOTW_MS_CLIENT_ID") {
    Some(v) => v,
    None => "",
};
