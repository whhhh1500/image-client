mod agent_prompts;
mod api;
mod assets;
mod comic;
mod comic_markdown;
mod comic_visual;
mod comic_visual_asset;
mod comic_visual_batch;
mod comic_visual_export;
mod comic_visual_render;
mod commands;
mod config;
mod db;
mod gateway;
#[cfg(feature = "real-e2e-harness")]
mod harness_transport;
mod history;
mod llm;
mod legacy_comic_retirement;
mod logging;
mod model;
mod novel;
mod novel_adaptation;
mod paths;
mod providers;
#[cfg(feature = "real-e2e-harness")]
mod real_e2e_harness;
mod util;
mod video;

use std::{
    io,
    path::{Component, Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, RwLock,
    },
};

use tauri::utils::config::WindowConfig;
use tauri::{Manager, RunEvent, WebviewWindowBuilder};

const DATA_DIR_OVERRIDE: &str = "IMAGE_CLIENT_DATA_DIR";

/// Shared state available to Tauri commands. Config is a snapshot pushed from
/// the frontend (which owns the SQLite `settings` table); app data & keys stay
/// server-side.
pub struct AppState {
    pub cfg: Arc<RwLock<config::ConfigState>>,
    pub registry: Arc<providers::ProviderRegistry>,
}

/// `RunEvent::Exit` is the reliable normal GUI-close lifecycle signal. Keep
/// the log idempotent because platform event loops may forward it more than
/// once while shutting down.
fn claim_application_exit_log(logged: &AtomicBool) -> bool {
    !logged.swap(true, Ordering::AcqRel)
}

fn api_server_exit_action(
    control: Option<api::ApiServerControl>,
    code: Option<i32>,
) -> Option<(api::ApiServerControl, api::ExitRequestAction)> {
    control.map(|control| {
        let action = control.request_exit(code == Some(tauri::RESTART_EXIT_CODE));
        (control, action)
    })
}

/// When the Rust data root is explicitly overridden, move only the windows
/// that Tauri would otherwise create automatically. The caller then creates
/// those windows with an explicit WebView2 data directory after the root has
/// been validated during setup.
fn take_auto_created_windows_for_isolation(
    windows: &mut [WindowConfig],
    data_dir_override_present: bool,
) -> Vec<WindowConfig> {
    if !data_dir_override_present {
        return Vec::new();
    }

    windows
        .iter_mut()
        .filter_map(|window| {
            if !window.create {
                return None;
            }
            let window_config = window.clone();
            window.create = false;
            Some(window_config)
        })
        .collect()
}

fn isolated_webview_data_directory(data_root: &Path, label: &str) -> io::Result<PathBuf> {
    if !data_root.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "validated application data root must be absolute",
        ));
    }
    let mut label_components = Path::new(label).components();
    let safe_label = !label.contains(['/', '\\'])
        && matches!(label_components.next(), Some(Component::Normal(_)))
        && label_components.next().is_none();
    if !safe_label {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "window label is not a safe WebView data-directory component",
        ));
    }

    Ok(data_root.join("webview2").join(label))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let mut context = tauri::generate_context!();
    #[cfg(feature = "real-e2e-harness")]
    let real_e2e_requested = real_e2e_harness::requested();
    #[cfg(not(feature = "real-e2e-harness"))]
    let real_e2e_requested = false;
    #[cfg(feature = "real-e2e-harness")]
    if real_e2e_requested {
        if real_e2e_harness::validate_bootstrap().is_err() {
            // Do this before paths/db setup so a malformed harness launch
            // cannot create or migrate a non-isolated database.
            std::process::exit(2);
        }
    }
    let isolated_windows = take_auto_created_windows_for_isolation(
        &mut context.config_mut().app.windows,
        std::env::var_os(DATA_DIR_OVERRIDE).is_some() || real_e2e_requested,
    );
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            // Ensure the fixed data directory + assets dir exist.
            paths::ensure_data_dirs()
                .map_err(|error| std::io::Error::other(format!("创建应用目录失败: {error}")))?;
            logging::init().map_err(std::io::Error::other)?;
            logging::install_panic_hook();
            let database = db::DbState::open(paths::data_dir().join("image-client.db"))?;
            logging::info(
                "application.start",
                serde_json::json!({
                    "name": app.package_info().name.to_string(),
                    "version": app.package_info().version.to_string(),
                    "platform": std::env::consts::OS,
                    "arch": std::env::consts::ARCH,
                    "dataDir": paths::data_dir().display().to_string(),
                    "dbPath": paths::data_dir().join("image-client.db").display().to_string(),
                    "logsDir": paths::logs_dir().display().to_string(),
                }),
            );

            let cfg = Arc::new(RwLock::new(config::ConfigState::load()));
            if real_e2e_requested {
                let status = cfg.read().unwrap().status();
                logging::info(
                    "configuration.loaded",
                    serde_json::json!({
                        "imageReady": status.image_ready,
                        "llmReady": status.llm_ready,
                        "videoReady": status.video_ready,
                        "source": status.source,
                    }),
                );
            } else {
                logging::info(
                    "configuration.loaded",
                    serde_json::to_value(cfg.read().unwrap().status()).unwrap_or_default(),
                );
            }

            let registry = Arc::new(providers::ProviderRegistry::new());
            registry.register(Arc::new(providers::GatewayProvider::new(cfg.clone())));
            let history_sync = Arc::new(history::HistorySyncState::default());

            app.manage(AppState {
                cfg: cfg.clone(),
                registry: registry.clone(),
            });
            let retired = legacy_comic_retirement::retire_pending(&database).map_err(std::io::Error::other)?;
            logging::info("legacy_comic.retired", serde_json::json!({"interruptedRecords": retired}));
            comic_markdown::recover_interrupted(&database).map_err(std::io::Error::other)?;
            app.manage(database);
            app.manage(history_sync.clone());
            if real_e2e_requested {
                #[cfg(feature = "real-e2e-harness")]
                real_e2e_harness::launch(app.handle().clone())?;
                return Ok(());
            }
            for window_config in &isolated_windows {
                let webview_data_directory = isolated_webview_data_directory(
                    &paths::data_dir(),
                    &window_config.label,
                )?;
                WebviewWindowBuilder::from_config(app.handle(), window_config)?
                    .data_directory(webview_data_directory)
                    .build()?;
            }
            // Keep the single REST server task reachable so normal exit can
            // stop accepting work before Tauri tears the process down.
            let api_server = api::ApiServerControl::new();
            let server_task = tauri::async_runtime::spawn(api::start(
                api_server.clone(),
                app.handle().clone(),
                cfg,
                registry,
                history_sync,
            ));
            api_server.set_task(server_task);
            app.manage(api_server);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_info,
            commands::data_dir,
            commands::logs_dir,
            commands::client_logs,
            commands::import_ref_image,
            commands::config_status,
            commands::save_config,
            commands::list_providers,
            commands::set_active_provider,
            commands::list_image_models,
            commands::list_video_models,
            commands::run_node,
            commands::run_video,
            commands::llm_chat,
            commands::agent_run,
            commands::save_text,
            commands::read_text_asset,
            commands::save_media_asset,
            commands::cache_promptlib_image,
            commands::list_cached_promptlib_images,
            commands::inspect_image_file,
            commands::compress_image_file,
            commands::convert_image_file,
            commands::cleanup_video_segments,
            history::mark_history_listener_ready,
            history::acknowledge_history_revision,
            db::db_execute,
            db::db_select,
            comic_markdown::comic_md_workspace_get,
            comic_markdown::comic_md_document_save,
            comic_markdown::comic_md_document_history,
            comic_markdown::comic_md_generate,
            comic_markdown::comic_md_optimize,
            comic_markdown::sync::comic_md_sync,
            comic_markdown::comic_md_render_options_save,
            comic_markdown::comic_md_render,
            comic_markdown::comic_md_export,
            novel::novel_work_create,
            novel::novel_work_list,
            novel::novel_work_get,
            novel::novel_work_archive,
            novel::novel_work_restore,
            novel::novel_volume_create,
            novel::novel_chapter_revision_create,
            novel::novel_snapshot,

        ]);
    let app = match builder.build(context) {
        Ok(app) => app,
        Err(error) => {
            logging::error(
                "application.exit",
                serde_json::json!({ "status": "error", "error": error.to_string() }),
            );
            panic!("error while running tauri application: {error}");
        }
    };
    let exit_logged = Arc::new(AtomicBool::new(false));
    app.run(move |app_handle, event| match event {
        RunEvent::ExitRequested { code, api, .. } => {
            let control = app_handle
                .try_state::<api::ApiServerControl>()
                .map(|state| state.inner().clone());
            if let Some((api_server, action)) = api_server_exit_action(control, code) {
                match action {
                    api::ExitRequestAction::PreventAndDrain => {
                        api.prevent_exit();
                        let control = api_server.clone();
                        let app_handle = app_handle.clone();
                        tauri::async_runtime::spawn(async move {
                            let result = control.drain_for_exit().await;
                            if !matches!(result, api::ApiDrainResult::Graceful) {
                                logging::warn(
                                    "api.server.shutdown_incomplete",
                                    serde_json::json!({ "result": format!("{result:?}") }),
                                );
                            }
                            app_handle.exit(code.unwrap_or(0));
                        });
                    }
                    api::ExitRequestAction::PreventWhileDraining => api.prevent_exit(),
                    api::ExitRequestAction::BestEffortRestart => api_server.signal_shutdown(),
                    api::ExitRequestAction::AllowExit => {}
                }
            }
        }
        RunEvent::Exit if claim_application_exit_log(&exit_logged) => {
            logging::info("application.exit", serde_json::json!({ "status": "ok" }));
        }
        _ => {}
    });
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::atomic::AtomicBool;

    use tauri::utils::config::WindowConfig;

    use super::{
        api_server_exit_action, claim_application_exit_log, isolated_webview_data_directory,
        take_auto_created_windows_for_isolation,
    };

    fn window_config(label: &str, create: bool) -> WindowConfig {
        let mut config = WindowConfig::default();
        config.label = label.into();
        config.create = create;
        config.title = format!("{label} title");
        config.width = 1234.0;
        config
    }

    #[test]
    fn application_exit_log_is_claimed_only_once() {
        let logged = AtomicBool::new(false);

        assert!(claim_application_exit_log(&logged));
        assert!(!claim_application_exit_log(&logged));
    }

    #[test]
    fn missing_api_server_state_leaves_exit_unintercepted() {
        assert!(api_server_exit_action(None, None).is_none());
    }

    #[test]
    fn absent_data_root_override_keeps_default_window_creation_unchanged() {
        let mut windows = vec![window_config("main", true), window_config("later", false)];

        let deferred = take_auto_created_windows_for_isolation(&mut windows, false);

        assert!(deferred.is_empty());
        assert!(windows[0].create);
        assert!(!windows[1].create);
    }

    #[test]
    fn data_root_override_defers_only_originally_auto_created_windows() {
        let mut windows = vec![window_config("main", true), window_config("later", false)];

        let deferred = take_auto_created_windows_for_isolation(&mut windows, true);

        assert_eq!(deferred.len(), 1);
        assert_eq!(deferred[0].label, "main");
        assert!(deferred[0].create);
        assert_eq!(deferred[0].title, "main title");
        assert_eq!(deferred[0].width, 1234.0);
        assert_eq!(deferred[0].url, windows[0].url);
        assert!(!windows[0].create);
        assert!(!windows[1].create);
    }

    #[test]
    fn invalid_profile_root_or_label_is_rejected_before_window_building() {
        let data_root = std::env::temp_dir();

        assert!(isolated_webview_data_directory(Path::new("relative-root"), "main").is_err());
        assert!(isolated_webview_data_directory(&data_root, "..").is_err());
        assert!(isolated_webview_data_directory(&data_root, "nested/main").is_err());
        assert!(isolated_webview_data_directory(&data_root, "nested\\main").is_err());
        assert_eq!(
            isolated_webview_data_directory(&data_root, "main").unwrap(),
            data_root.join("webview2").join("main"),
        );
    }
}
