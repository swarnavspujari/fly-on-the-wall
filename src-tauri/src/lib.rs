//! Composition root: this is the ONLY place where platform impls are picked
//! and wired to the UI. `fly-core` and the frontend never see an OS API.

mod asr_commands;
mod calendar_commands;
mod calendar_defaults;
mod commands;
pub mod extraction;
pub mod gpu;
pub mod hw;
mod import_commands;
mod live;
mod llm_commands;
pub mod models;
pub mod ollama;
pub mod pipeline;
pub mod recording;
pub mod scheduler;
mod screen_commands;
pub mod state;

use tauri::Manager;

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

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
        .plugin(tauri_plugin_clipboard_manager::init())
        .setup(|app| {
            // Updater + relaunch only exist on desktop targets.
            #[cfg(desktop)]
            {
                app.handle()
                    .plugin(tauri_plugin_updater::Builder::new().build())?;
                app.handle().plugin(tauri_plugin_process::init())?;
            }
            // A failed storage open must NEVER be a silent flash-crash: the
            // window closes before the user can read anything and it looks
            // like the app is broken (seen in the wild with a database that
            // was replaced under a live connection). Tell them what happened
            // and that their data folder is intact, then exit cleanly.
            let app_state = match state::AppState::init() {
                Ok(s) => s,
                Err(e) => {
                    use tauri_plugin_dialog::{DialogExt, MessageDialogKind};
                    tracing::error!(error = %e, "storage failed to open");
                    app.dialog()
                        .message(format!(
                            "Fly on the Wall couldn't open its database:\n\n{e}\n\n\
                             Your recordings, notes, and transcripts are separate \
                             files and are intact in the app data folder. Close any \
                             other program that may be using the database (or restore \
                             a backup of flyonthewall.db) and start the app again."
                        ))
                        .kind(MessageDialogKind::Error)
                        .title("Fly on the Wall can't start")
                        .blocking_show();
                    std::process::exit(1);
                }
            };
            // Let the webview stream recordings from the data dir so the editor's
            // audio player can embed & scrub them (asset protocol).
            let _ = app
                .asset_protocol_scope()
                .allow_directory(&app_state.data_dir, true);
            app.manage(app_state);
            // Warm the hardware cache off the startup path: detection shells
            // out to nvidia-smi and must never sit in front of first paint.
            {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    let _ = tauri::async_runtime::spawn_blocking(move || {
                        let state = handle.state::<state::AppState>();
                        hw::detect_and_cache(&state.storage.lock().unwrap());
                    })
                    .await;
                });
            }
            // Recording self-heal: re-attach (or fully resurrect) finished
            // recordings whose database write was lost — the manifests written
            // at stop time make this possible even after the database itself
            // was replaced. Off the startup path; a healthy install no-ops.
            {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    let _ = tauri::async_runtime::spawn_blocking(move || {
                        let state = handle.state::<state::AppState>();
                        let storage = state.storage.lock().unwrap();
                        match storage.self_heal_recordings() {
                            Ok(r) if !r.is_empty() => tracing::info!(
                                attached = r.attached.len(),
                                resurrected = r.resurrected.len(),
                                "recording self-heal repaired meetings"
                            ),
                            Ok(_) => {}
                            Err(e) => {
                                tracing::warn!(error = %e, "recording self-heal failed")
                            }
                        }
                    })
                    .await;
                });
            }
            // Drain queued transcriptions (incl. jobs surviving a restart)
            // whenever no recording is active.
            scheduler::spawn(app.handle().clone());
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
            commands::copy_note_markdown,
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
            asr_commands::get_cleaned_transcript,
            asr_commands::relabel_speaker,
            asr_commands::edit_transcript_segment,
            asr_commands::update_meeting_attendees,
            asr_commands::re_diarize_meeting,
            asr_commands::revert_speaker_assignment,
            asr_commands::speaker_undo_state,
            asr_commands::pipeline_stage,
            asr_commands::get_asr_settings,
            asr_commands::set_asr_settings,
            asr_commands::download_model,
            llm_commands::enhance_note,
            llm_commands::polish_transcript,
            llm_commands::edit_note_block,
            llm_commands::ask_meeting,
            llm_commands::list_templates,
            llm_commands::save_template,
            llm_commands::delete_template,
            llm_commands::get_llm_settings,
            llm_commands::set_llm_settings,
            llm_commands::test_llm_connection,
            extraction::extract_meeting_items,
            extraction::backfill_meeting_items,
            ollama::ollama_status,
            ollama::ollama_pull,
            calendar_commands::get_calendar_settings,
            calendar_commands::set_calendar_settings,
            calendar_commands::connect_calendar,
            calendar_commands::disconnect_calendar,
            calendar_commands::upcoming_meetings,
            calendar_commands::list_calendars,
            calendar_commands::set_calendar_enabled,
            calendar_commands::start_meeting_from_event,
            screen_commands::screen_status,
            screen_commands::start_screen_recording,
            screen_commands::stop_screen_recording,
            screen_commands::ensure_video_thumbnail,
            import_commands::import_media,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Fly on the Wall")
        .run(|app, event| {
            // The managed `ollama serve` child must not outlive the app.
            if let tauri::RunEvent::Exit = event {
                ollama::shutdown(&app.state::<state::AppState>());
            }
        });
}
