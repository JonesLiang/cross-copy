mod core;
mod crypto;
mod logger;
mod model;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod mouse_hook;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod mouse_share;
mod remote_fs;
mod store;

use crate::core::Core;
use serde::Serialize;
#[cfg(target_os = "macos")]
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager, State, WebviewUrl, WebviewWindowBuilder,
};
use tauri_plugin_autostart::MacosLauncher;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

#[tauri::command]
fn get_state(core: State<'_, Arc<Core>>) -> model::UiState {
    core.ui_state()
}

#[tauri::command]
fn begin_pairing(core: State<'_, Arc<Core>>) {
    core.begin_pairing();
}

#[tauri::command]
fn cancel_pairing(core: State<'_, Arc<Core>>) {
    core.cancel_pairing();
}

#[tauri::command]
async fn submit_pairing_code(core: State<'_, Arc<Core>>, code: String) -> Result<(), String> {
    Arc::clone(core.inner()).pair_with_code(code).await
}

#[tauri::command]
fn set_sync_enabled(core: State<'_, Arc<Core>>, value: bool) -> Result<(), String> {
    core.set_sync(value)
}

#[tauri::command]
async fn set_mouse_share_enabled(core: State<'_, Arc<Core>>, value: bool) -> Result<(), String> {
    Arc::clone(core.inner())
        .set_mouse_share_enabled(value)
        .await
}

#[tauri::command]
fn set_mouse_extreme_performance(core: State<'_, Arc<Core>>, value: bool) -> Result<(), String> {
    core.set_mouse_extreme_performance(value)
}

#[tauri::command]
async fn set_mouse_position(
    core: State<'_, Arc<Core>>,
    position: model::ScreenPosition,
) -> Result<(), String> {
    Arc::clone(core.inner()).set_mouse_position(position).await
}

#[tauri::command]
async fn set_peer_screen_position(
    core: State<'_, Arc<Core>>,
    peer_id: String,
    position: model::ScreenPosition,
) -> Result<(), String> {
    Arc::clone(core.inner())
        .set_peer_screen_position(peer_id, position)
        .await
}

#[tauri::command]
async fn set_peer_permissions(
    core: State<'_, Arc<Core>>,
    peer_id: String,
    clipboard_allowed: bool,
    mouse_allowed: bool,
    filesystem_allowed: bool,
) -> Result<(), String> {
    Arc::clone(core.inner())
        .set_peer_permissions(
            peer_id,
            clipboard_allowed,
            mouse_allowed,
            filesystem_allowed,
        )
        .await
}

#[tauri::command]
async fn filesystem_request(
    core: State<'_, Arc<Core>>,
    peer_id: String,
    request: remote_fs::FsRequest,
) -> Result<remote_fs::FsResponse, String> {
    Arc::clone(core.inner())
        .filesystem_request(peer_id, request)
        .await
}

#[tauri::command]
async fn filesystem_download(
    core: State<'_, Arc<Core>>,
    peer_id: String,
    paths: Vec<String>,
) -> Result<String, String> {
    Arc::clone(core.inner())
        .filesystem_download(peer_id, paths)
        .await
}

#[tauri::command]
async fn filesystem_upload(
    core: State<'_, Arc<Core>>,
    peer_id: String,
    local_paths: Vec<String>,
    target_dir: String,
) -> Result<(), String> {
    Arc::clone(core.inner())
        .filesystem_upload(peer_id, local_paths, target_dir)
        .await
}

#[tauri::command]
async fn filesystem_upload_clipboard(
    core: State<'_, Arc<Core>>,
    peer_id: String,
    target_dir: String,
) -> Result<(), String> {
    Arc::clone(core.inner())
        .filesystem_upload_clipboard(peer_id, target_dir)
        .await
}

#[tauri::command]
fn set_peer_mouse_dpi(core: State<'_, Arc<Core>>, peer_id: String, dpi: u16) -> Result<(), String> {
    core.set_peer_mouse_dpi(peer_id, dpi)
}

#[tauri::command]
fn switch_mouse_to_screen(core: State<'_, Arc<Core>>, screen_number: u8) -> Result<(), String> {
    core.switch_mouse_to_screen(screen_number)
}

#[tauri::command]
fn set_launch_at_login(
    app: tauri::AppHandle,
    core: State<'_, Arc<Core>>,
    value: bool,
) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt;
    if value {
        app.autolaunch().enable().map_err(|e| e.to_string())?;
    } else {
        app.autolaunch().disable().map_err(|e| e.to_string())?;
    }
    core.set_launch_at_login(value)
}

#[tauri::command]
fn unpair(core: State<'_, Arc<Core>>, peer_id: String) -> Result<(), String> {
    core.unpair(&peer_id)
}

#[tauri::command]
fn export_diagnostics(core: State<'_, Arc<Core>>) -> Result<String, String> {
    core.export_diagnostics()
}

#[tauri::command]
fn wake_network(core: State<'_, Arc<Core>>) {
    core.wake_network();
}

#[tauri::command]
fn open_input_permissions() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
            .spawn()
            .map_err(|error| format!("无法打开辅助功能设置：{error}"))?;
    }
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateEnvironment {
    ready: bool,
    reason: Option<String>,
}

#[tauri::command]
fn get_update_environment(core: State<'_, Arc<Core>>) -> UpdateEnvironment {
    let (environment, location) = update_environment();
    core.log_update_event(
        if environment.ready { "info" } else { "warn" },
        "environment",
        &format!("ready={} location={location}", environment.ready),
    );
    environment
}

#[tauri::command]
fn log_update_event(core: State<'_, Arc<Core>>, level: String, event: String, detail: String) {
    core.log_update_event(&level, &event, &detail);
}

#[tauri::command]
fn set_shortcuts(
    app: tauri::AppHandle,
    core: State<'_, Arc<Core>>,
    copy: String,
    paste: String,
    mouse: String,
) -> Result<(), String> {
    let copy = copy.trim().to_string();
    let paste = paste.trim().to_string();
    let mouse = mouse.trim().to_string();
    let copy_parsed: Shortcut = copy
        .parse()
        .map_err(|error| format!("复制快捷键无效：{error}"))?;
    let paste_parsed: Shortcut = paste
        .parse()
        .map_err(|error| format!("粘贴快捷键无效：{error}"))?;
    let mouse_parsed: Shortcut = mouse
        .parse()
        .map_err(|error| format!("鼠标共享快捷键无效：{error}"))?;
    if copy_parsed == paste_parsed || copy_parsed == mouse_parsed || paste_parsed == mouse_parsed {
        return Err("三个快捷键不能重复".into());
    }
    let reserved = [
        "ctrl+c",
        "ctrl+v",
        "command+c",
        "command+v",
        "cmd+c",
        "cmd+v",
    ];
    if reserved.contains(&copy.to_ascii_lowercase().as_str())
        || reserved.contains(&paste.to_ascii_lowercase().as_str())
        || reserved.contains(&mouse.to_ascii_lowercase().as_str())
    {
        return Err("不能占用系统原生复制或粘贴快捷键，请增加 Shift 或 Alt".into());
    }
    if [copy.as_str(), paste.as_str(), mouse.as_str()]
        .iter()
        .any(|value| screen_shortcuts().iter().any(|screen| screen == value))
    {
        return Err("Ctrl+Shift+1 至 Ctrl+Shift+9 已用于切换逻辑屏幕".into());
    }

    let previous = core.store.get();
    let _ = app.global_shortcut().unregister_multiple([
        previous.copy_shortcut.as_str(),
        previous.paste_shortcut.as_str(),
        previous.mouse_shortcut.as_str(),
    ]);
    if let Err(error) =
        app.global_shortcut()
            .register_multiple([copy.as_str(), paste.as_str(), mouse.as_str()])
    {
        let _ = app.global_shortcut().unregister_multiple([
            copy.as_str(),
            paste.as_str(),
            mouse.as_str(),
        ]);
        let _ = app.global_shortcut().register_multiple([
            previous.copy_shortcut.as_str(),
            previous.paste_shortcut.as_str(),
            previous.mouse_shortcut.as_str(),
        ]);
        return Err(format!("快捷键已被其他应用占用或无法注册：{error}"));
    }
    core.set_shortcuts(copy, paste, mouse)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let updater = tauri_plugin_updater::Builder::new();
    #[cfg(target_os = "macos")]
    let updater = updater.target("darwin-universal");

    tauri::Builder::default()
        .plugin(tauri_plugin_process::init())
        .plugin(updater.build())
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    if event.state != ShortcutState::Released {
                        return;
                    }
                    let core = app.state::<Arc<Core>>();
                    let settings = core.store.get();
                    let is_copy = settings
                        .copy_shortcut
                        .parse::<Shortcut>()
                        .is_ok_and(|value| &value == shortcut);
                    let is_paste = settings
                        .paste_shortcut
                        .parse::<Shortcut>()
                        .is_ok_and(|value| &value == shortcut);
                    let is_mouse = settings
                        .mouse_shortcut
                        .parse::<Shortcut>()
                        .is_ok_and(|value| &value == shortcut);
                    let screen_number =
                        screen_shortcuts()
                            .iter()
                            .enumerate()
                            .find_map(|(index, value)| {
                                value
                                    .parse::<Shortcut>()
                                    .is_ok_and(|parsed| &parsed == shortcut)
                                    .then_some((index + 1) as u8)
                            });
                    if is_copy {
                        let core = Arc::clone(core.inner());
                        tauri::async_runtime::spawn(async move {
                            core.trigger_copy().await;
                        });
                    } else if is_paste {
                        let core = Arc::clone(core.inner());
                        tauri::async_runtime::spawn(async move {
                            core.trigger_paste().await;
                        });
                    } else if is_mouse {
                        let core = Arc::clone(core.inner());
                        tauri::async_runtime::spawn(async move {
                            core.toggle_mouse_share().await;
                        });
                    } else if let Some(screen_number) = screen_number {
                        if let Err(error) = core.switch_mouse_to_screen(screen_number) {
                            core.log_mouse_shortcut_error(screen_number, &error);
                        }
                    }
                })
                .build(),
        )
        .setup(|app| {
            use tauri_plugin_autostart::ManagerExt;

            let app_dir = app.path().app_data_dir()?;
            let store = Arc::new(store::Store::load(app_dir.clone())?);
            let logger = Arc::new(logger::Logger::new(&app_dir)?);
            let core = Core::new(store, Arc::clone(&logger), app.handle().clone());
            app.manage(Arc::clone(&core));
            let shortcuts = core.store.get();
            if shortcuts.launch_at_login {
                let _ = app.autolaunch().disable();
                if let Err(error) = app.autolaunch().enable() {
                    logger.warn("update_autostart_refresh_failed", error.to_string());
                } else {
                    logger.info("update_autostart_refreshed", "target=current_executable");
                }
            }
            let registered = [
                shortcuts.copy_shortcut,
                shortcuts.paste_shortcut,
                shortcuts.mouse_shortcut,
            ];
            if let Err(error) = app
                .global_shortcut()
                .register_multiple(registered.iter().map(String::as_str))
            {
                logger.error("shortcut_registration_failed", error.to_string());
            }
            for shortcut in screen_shortcuts() {
                if let Err(error) = app.global_shortcut().register(shortcut) {
                    logger.warn(
                        "mouse_screen_shortcut_registration_failed",
                        format!("shortcut={shortcut} error={error}"),
                    );
                }
            }
            let service_logger = Arc::clone(&logger);
            tauri::async_runtime::spawn(async move {
                if let Err(error) = core.start().await {
                    service_logger.error("service_start_failed", error);
                }
            });
            let open = MenuItem::with_id(app, "open", "打开 CrossCopy", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &quit])?;
            TrayIconBuilder::new()
                .icon(app.default_window_icon().expect("app icon").clone())
                .tooltip("CrossCopy")
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => show_or_create_window(app),
                    "quit" => {
                        app.state::<Arc<Core>>().shutdown();
                        app.exit(0);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if matches!(
                        event,
                        TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        }
                    ) {
                        show_or_create_window(tray.app_handle());
                    }
                })
                .build(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if should_hide_on_close(window.label()) {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            begin_pairing,
            cancel_pairing,
            submit_pairing_code,
            set_sync_enabled,
            set_mouse_share_enabled,
            set_mouse_extreme_performance,
            set_mouse_position,
            set_peer_screen_position,
            set_peer_permissions,
            filesystem_request,
            filesystem_download,
            filesystem_upload,
            filesystem_upload_clipboard,
            set_peer_mouse_dpi,
            switch_mouse_to_screen,
            set_launch_at_login,
            unpair,
            export_diagnostics,
            wake_network,
            open_input_permissions,
            get_update_environment,
            log_update_event,
            set_shortcuts
        ])
        .run(tauri::generate_context!())
        .expect("failed to run CrossCopy");
}

fn update_environment() -> (UpdateEnvironment, &'static str) {
    #[cfg(target_os = "macos")]
    {
        let executable = std::env::current_exe().unwrap_or_default();
        let bundle = macos_bundle_path(&executable);
        let translocated = executable.to_string_lossy().contains("/AppTranslocation/");
        let installed = bundle.as_ref().is_some_and(|path| {
            path.starts_with("/Applications")
                || dirs::home_dir()
                    .map(|home| path.starts_with(home.join("Applications")))
                    .unwrap_or(false)
        });
        let location = if translocated {
            "app_translocation"
        } else if installed {
            "applications"
        } else {
            "portable"
        };
        if translocated || !installed {
            return (
                UpdateEnvironment {
                    ready: false,
                    reason: Some(
                        "当前运行的是下载目录或 macOS 隔离副本。请完全退出 CrossCopy，将 DMG 中的 CrossCopy 拖入“应用程序”，再从“应用程序”启动后更新。"
                            .into(),
                    ),
                },
                location,
            );
        }
        return (
            UpdateEnvironment {
                ready: true,
                reason: None,
            },
            location,
        );
    }

    #[cfg(not(target_os = "macos"))]
    (
        UpdateEnvironment {
            ready: true,
            reason: None,
        },
        "installed",
    )
}

#[cfg(target_os = "macos")]
fn macos_bundle_path(executable: &Path) -> Option<PathBuf> {
    executable
        .ancestors()
        .find(|path| path.extension().is_some_and(|extension| extension == "app"))
        .map(Path::to_path_buf)
}

fn screen_shortcuts() -> [&'static str; 9] {
    [
        "Ctrl+Shift+1",
        "Ctrl+Shift+2",
        "Ctrl+Shift+3",
        "Ctrl+Shift+4",
        "Ctrl+Shift+5",
        "Ctrl+Shift+6",
        "Ctrl+Shift+7",
        "Ctrl+Shift+8",
        "Ctrl+Shift+9",
    ]
}

fn show_or_create_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }
    let _ = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
        .title("CrossCopy")
        .inner_size(920.0, 680.0)
        .min_inner_size(760.0, 580.0)
        .center()
        .build();
}

fn should_hide_on_close(label: &str) -> bool {
    label == "main"
}

#[cfg(test)]
mod tests {
    use super::should_hide_on_close;

    #[test]
    fn only_main_window_stays_alive_when_closed() {
        assert!(should_hide_on_close("main"));
        assert!(!should_hide_on_close("transfer"));
    }
}
