//! Composition root: this is the ONLY place where platform impls are picked
//! and wired to the UI. `looma-core` and the frontend never see an OS API.

mod asr_commands;
mod calendar_commands;
mod commands;
pub mod hw;
mod import_commands;
mod live;
mod llm_commands;
pub mod models;
pub mod pipeline;
mod recording;
mod screen_commands;
pub mod state;

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
            commands::export_note,
            commands::remove_attachment,
            commands::open_attachment,
            commands::reveal_attachment,
            commands::reveal_data_dir,
            commands::mcp_config,
            commands::get_app_setting,
            commands::set_app_setting,
            commands::search,
            recording::recording_status,
            recording::start_recording,
            recording::pause_recording,
            recording::resume_recording,
            recording::stop_recording,
            recording::get_meeting_for_note,
            recording::list_mic_devices,
            asr_commands::transcribe_meeting,
            asr_commands::get_transcript,
            asr_commands::relabel_speaker,
            asr_commands::pipeline_stage,
            asr_commands::get_asr_settings,
            asr_commands::set_asr_settings,
            asr_commands::download_model,
            llm_commands::enhance_note,
            llm_commands::edit_note_block,
            llm_commands::ask_meeting,
            llm_commands::list_templates,
            llm_commands::save_template,
            llm_commands::delete_template,
            llm_commands::get_llm_settings,
            llm_commands::set_llm_settings,
            llm_commands::test_llm_connection,
            calendar_commands::get_calendar_settings,
            calendar_commands::set_calendar_settings,
            calendar_commands::connect_calendar,
            calendar_commands::disconnect_calendar,
            calendar_commands::upcoming_meetings,
            calendar_commands::start_meeting_from_event,
            screen_commands::screen_status,
            screen_commands::start_screen_recording,
            screen_commands::stop_screen_recording,
            import_commands::import_media,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Looma");
}
