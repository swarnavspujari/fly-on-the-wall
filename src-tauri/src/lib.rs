//! Composition root: this is the ONLY place where platform impls are picked
//! and wired to the UI. `looma-core` and the frontend never see an OS API.

mod commands;
mod state;

use tauri::Manager;

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    tauri::Builder::default()
        .setup(|app| {
            let app_state = state::AppState::init()?;
            app.manage(app_state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![commands::ping, commands::app_info,])
        .run(tauri::generate_context!())
        .expect("error while running Looma");
}
