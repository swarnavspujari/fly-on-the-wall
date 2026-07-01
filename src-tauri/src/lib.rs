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
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let app_state = state::AppState::init()?;
            app.manage(app_state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::ping,
            commands::app_info,
            commands::list_folders,
            commands::create_folder,
            commands::rename_folder,
            commands::move_folder,
            commands::delete_folder,
            commands::create_note,
            commands::get_note,
            commands::list_notes_in_folder,
            commands::list_recent_notes,
            commands::update_note_title,
            commands::update_note_scratchpad,
            commands::move_note,
            commands::delete_note,
            commands::attach_file,
            commands::remove_attachment,
            commands::open_attachment,
            commands::reveal_attachment,
            commands::reveal_data_dir,
            commands::search,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Looma");
}
