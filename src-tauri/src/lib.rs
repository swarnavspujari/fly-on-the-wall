//! Composition root: this is the ONLY place where platform impls are picked
//! and wired to the UI. `fly-core` and the frontend never see an OS API.

mod asr_commands;
pub mod audio_store;
mod calendar_commands;
mod calendar_defaults;
mod commands;
pub mod connectors;
pub mod embeddings;
pub mod extraction;
pub mod gpu;
pub mod hw;
mod import_commands;
mod live;
pub mod llm_commands;
pub mod logging;
pub mod model_cleanup;
pub mod models;
mod nav_guard;
pub mod ollama;
pub mod pipeline;
pub mod privacy;
pub mod recording;
pub mod scheduler;
mod screen_commands;
mod startup;
pub mod state;

use tauri::Manager;

pub fn run() {
    // Resolve (and legacy-migrate) the data dir BEFORE file logging exists:
    // logging::init creating `FlyOnTheWall/logs` first would make the
    // migration think it already ran. A migration failure falls back to
    // stdout-only logging here and surfaces via the startup dialog below
    // (AppState::init repeats the same idempotent preparation).
    let data_dir = state::prepared_data_dir();
    // Take the per-data-dir instance lock and finish a pending "Delete all
    // my data" (only once the instance that asked for it has exited) —
    // before logging or the database open any file in the data dir.
    if let Ok(dir) = data_dir.as_deref() {
        privacy::startup(dir);
    }
    logging::init(data_dir.as_deref().ok());

    let mut builder = tauri::Builder::default();

    // Single-instance must be the FIRST plugin registered: a second launch from
    // the desktop shortcut then focuses the already-open window instead of
    // spawning another process. Desktop-only, like the updater/process plugins.
    #[cfg(desktop)]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // Runs inside the already-running instance when a second launch is
            // attempted; bring the existing "main" window forward (robust even
            // if it was minimized or hidden — unminimize/show before focusing).
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }));
    }

    builder
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        // The app window only ever shows the app; links open in the browser.
        .plugin(nav_guard::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .setup(|app| {
            // Updater + relaunch only exist on desktop targets.
            #[cfg(desktop)]
            {
                app.handle()
                    .plugin(tauri_plugin_updater::Builder::new().build())?;
                app.handle().plugin(tauri_plugin_process::init())?;
            }
            let handle = app.handle().clone();
            // A first launch after a schema update can take tens of seconds
            // on a large library: open it off the main thread so the window
            // shows the upgrade screen instead of freezing (startup.rs).
            if startup::upgrade_pending() {
                startup::open_in_background(handle);
                return Ok(());
            }
            app.manage(startup::LibraryGate::ready());
            let app_state = match state::AppState::init() {
                Ok(s) => s,
                Err(e) => startup::storage_failed(&handle, &e),
            };
            startup::finish(&handle, app_state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            startup::library_status,
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
            commands::cancel_transcription,
            commands::attach_file,
            commands::export_note,
            commands::export_transcript_vtt,
            commands::copy_note_markdown,
            commands::remove_attachment,
            commands::open_attachment,
            commands::open_external_url,
            commands::reveal_attachment,
            commands::reveal_data_dir,
            commands::reveal_logs_dir,
            commands::mcp_config,
            commands::get_app_setting,
            commands::set_app_setting,
            commands::search,
            commands::search_semantic,
            recording::recording_status,
            recording::start_recording,
            recording::pause_recording,
            recording::resume_recording,
            recording::stop_recording,
            recording::get_meeting_for_note,
            recording::get_meetings_for_note,
            recording::update_meeting_started_at,
            recording::list_mic_devices,
            recording::preflight_system_audio,
            recording::open_privacy_settings,
            live::live_status,
            audio_store::prepare_playback,
            audio_store::storage_report,
            audio_store::dismiss_compress_notice,
            asr_commands::transcribe_meeting,
            asr_commands::get_transcript,
            asr_commands::get_cleaned_transcript,
            asr_commands::relabel_speaker,
            asr_commands::edit_transcript_segment,
            asr_commands::update_meeting_attendees,
            asr_commands::re_diarize_meeting,
            asr_commands::revert_speaker_assignment,
            asr_commands::speaker_undo_state,
            asr_commands::pipeline_stage,
            asr_commands::pipeline_failure,
            asr_commands::get_asr_settings,
            asr_commands::set_asr_settings,
            asr_commands::download_model,
            model_cleanup::model_storage_report,
            model_cleanup::delete_model_files,
            llm_commands::enhance_note,
            llm_commands::polish_transcript,
            llm_commands::edit_note_block,
            llm_commands::keep_note_block,
            llm_commands::ask_meeting,
            llm_commands::list_templates,
            llm_commands::save_template,
            llm_commands::delete_template,
            llm_commands::get_llm_settings,
            llm_commands::set_llm_settings,
            llm_commands::test_llm_connection,
            llm_commands::consent_skipped_meetings,
            llm_commands::retry_consent_skipped,
            llm_commands::request_cloud_consent,
            extraction::extract_meeting_items,
            extraction::backfill_meeting_items,
            extraction::pending_backfill_count,
            ollama::ollama_status,
            ollama::ollama_pull,
            ollama::ollama_delete,
            calendar_commands::get_calendar_settings,
            calendar_commands::set_calendar_settings,
            calendar_commands::connect_calendar,
            calendar_commands::disconnect_calendar,
            calendar_commands::upcoming_meetings,
            calendar_commands::list_calendars,
            calendar_commands::set_calendar_enabled,
            calendar_commands::start_meeting_from_event,
            screen_commands::screen_status,
            screen_commands::list_capture_windows,
            screen_commands::start_screen_recording,
            screen_commands::stop_screen_recording,
            screen_commands::ensure_video_thumbnail,
            import_commands::import_stage,
            import_commands::import_state,
            import_commands::import_transcribe,
            privacy::get_meeting_privacy,
            privacy::set_note_keep_local,
            privacy::set_folder_keep_local,
            privacy::list_local_only_folders,
            privacy::set_participants_notified,
            privacy::copy_recording_notice,
            privacy::get_privacy_settings,
            privacy::preview_audio_retention,
            privacy::set_audio_retention,
            privacy::dismiss_wipe_report,
            privacy::delete_all_data,
            privacy::disk_encryption_status,
            privacy::recovered_kept_local,
            privacy::dismiss_recovered_kept_local,
            connectors::connectors_overview,
            connectors::connectors_directory,
            connectors::add_connector,
            connectors::update_connector,
            connectors::set_connector_enabled,
            connectors::remove_connector,
            connectors::sync_connector_tools,
            connectors::set_connector_tool_permissions,
            connectors::approve_connector_tool,
            connectors::sign_in_connector,
            connectors::sign_out_connector,
            connectors::claude_desktop_candidates,
            connectors::import_claude_desktop_connector,
            connectors::connector_call_log,
            connectors::clear_connector_call_log,
            connectors::pending_connector_calls,
            connectors::decide_connector_call,
            connectors::test_connector_tool,
            connectors::pick_connector_program,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Fly on the Wall")
        .run(|app, event| {
            // The managed `ollama serve` child must not outlive the app.
            // `try_state`: quitting during a startup upgrade exits before
            // AppState exists (the open's transaction just rolls back).
            if let tauri::RunEvent::Exit = event {
                if let Some(state) = app.try_state::<state::AppState>() {
                    ollama::shutdown(&state);
                }
                // Stdio connector programs stop with the app.
                connectors::shutdown(app);
            }
        });
}
