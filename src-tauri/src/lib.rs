mod core;
mod crypto;
mod model;
mod store;

use crate::core::Core;
use std::sync::Arc;
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager, State, WebviewUrl, WebviewWindowBuilder,
};
use tauri_plugin_autostart::MacosLauncher;

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
async fn submit_pairing_code(
    core: State<'_, Arc<Core>>,
    code: String,
) -> Result<(), String> {
    Arc::clone(core.inner()).pair_with_code(code).await
}

#[tauri::command]
fn set_sync_enabled(core: State<'_, Arc<Core>>, value: bool) -> Result<(), String> {
    core.set_sync(value)
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
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
        .setup(|app| {
            let app_dir = app.path().app_data_dir()?;
            let store = Arc::new(store::Store::load(app_dir)?);
            let core = Core::new(store, app.handle().clone());
            app.manage(Arc::clone(&core));
            tauri::async_runtime::spawn(async move {
                if let Err(error) = core.start().await {
                    eprintln!("CrossCopy service failed: {error}");
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
                    "quit" => app.exit(0),
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
                api.prevent_close();
                let window = window.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    let _ = window.destroy();
                });
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            begin_pairing,
            cancel_pairing,
            submit_pairing_code,
            set_sync_enabled,
            set_launch_at_login,
            unpair
        ])
        .run(tauri::generate_context!())
        .expect("failed to run CrossCopy");
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
