mod activity;
mod bridge;
mod claude;
mod codex;
mod gemini;
mod grok;
mod native_host;
mod quota;
mod startup;

use quota::ProviderQuota;
use std::sync::Mutex;
use tauri::{
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, LogicalSize, Manager, PhysicalPosition, PhysicalSize, State, WebviewWindow,
    WindowEvent,
};

const MAIN_WINDOW: &str = "main";
const MINI_CONTENT_WIDTH: f64 = 300.0;

#[derive(Default)]
struct WindowModeState {
    normal_bounds: Mutex<Option<(PhysicalSize<u32>, PhysicalPosition<i32>)>>,
}

pub fn run_native_host_if_requested() -> bool {
    let Some(origin) = std::env::args().nth(1) else {
        return false;
    };
    if !origin.starts_with("chrome-extension://") {
        return false;
    }

    if let Err(error) = native_host::run(&origin) {
        eprintln!("native host stopped: {error}");
    }
    true
}

#[tauri::command]
async fn claude_quota() -> ProviderQuota {
    claude::fetch().await
}

#[tauri::command]
async fn codex_quota() -> ProviderQuota {
    codex::fetch().await
}

#[tauri::command]
async fn grok_quota() -> ProviderQuota {
    grok::fetch().await
}

#[tauri::command]
async fn gemini_quota() -> ProviderQuota {
    gemini::fetch().await
}

#[tauri::command]
fn system_activity() -> activity::SystemActivity {
    activity::snapshot()
}

/// The folder the user points `Load unpacked` at. Surfaced so the setup panel
/// and the README never have to guess where the installer landed.
#[tauri::command]
fn bridge_dir() -> Result<String, String> {
    bridge::install_dir().map(|dir| dir.display().to_string())
}

#[tauri::command]
fn reveal_bridge_dir() -> Result<(), String> {
    bridge::reveal()
}

#[tauri::command]
fn set_mini_mode(
    window: WebviewWindow,
    state: State<'_, WindowModeState>,
    enabled: bool,
    height: f64,
) -> Result<(), String> {
    let mut normal_bounds = state
        .normal_bounds
        .lock()
        .map_err(|_| "window mode state is unavailable".to_string())?;

    if !enabled {
        if let Some((size, position)) = normal_bounds.take() {
            window.set_size(size).map_err(|error| error.to_string())?;
            window
                .set_position(position)
                .map_err(|error| error.to_string())?;
        }
        return Ok(());
    }

    if normal_bounds.is_none() {
        let size = window.outer_size().map_err(|error| error.to_string())?;
        let position = window.outer_position().map_err(|error| error.to_string())?;
        *normal_bounds = Some((size, position));
    }
    drop(normal_bounds);

    // The renderer measures client content, while Tauri resizes the outer
    // window. Preserve the current frame/title-bar thickness at any DPI.
    let scale = window.scale_factor().map_err(|error| error.to_string())?;
    let outer = window.outer_size().map_err(|error| error.to_string())?;
    let inner = window.inner_size().map_err(|error| error.to_string())?;
    let frame_width = outer.width.saturating_sub(inner.width) as f64 / scale;
    let frame_height = outer.height.saturating_sub(inner.height) as f64 / scale;
    let content_height = if height.is_finite() {
        height.clamp(72.0, 420.0)
    } else {
        180.0
    };

    window
        .set_size(LogicalSize::new(
            MINI_CONTENT_WIDTH + frame_width,
            content_height + frame_height,
        ))
        .map_err(|error| error.to_string())
}

fn show_dashboard(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Hide the dashboard only when it is the window the user is actually looking
/// at. Testing visibility alone would hide a window sitting behind the browser,
/// so a click meant to reveal the deck would appear to do nothing.
fn toggle_dashboard(app: &AppHandle) {
    let Some(window) = app.get_webview_window(MAIN_WINDOW) else {
        return;
    };
    let in_front = window.is_visible().unwrap_or(false) && window.is_focused().unwrap_or(false);
    if in_front {
        let _ = window.hide();
    } else {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

#[cfg(desktop)]
fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open dashboard", true, None::<&str>)?;
    let launch_at_startup = CheckMenuItem::with_id(
        app,
        "launch-at-startup",
        "Launch at startup",
        true,
        startup::enabled(),
        None::<&str>,
    )?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &open,
            &PredefinedMenuItem::separator(app)?,
            &launch_at_startup,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;
    let startup_toggle = launch_at_startup.clone();

    TrayIconBuilder::with_id("main-tray")
        .icon(app.default_window_icon().unwrap().clone())
        .tooltip("AI Quota Deck")
        .menu(&menu)
        // Left click toggles the dashboard, so the menu belongs on right click only.
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| match event.id.as_ref() {
            "open" => show_dashboard(app),
            "launch-at-startup" => {
                let enable = !startup::enabled();
                match startup::set_enabled(enable) {
                    Ok(()) => {
                        let _ = startup_toggle.set_checked(enable);
                    }
                    Err(error) => {
                        eprintln!("launch at startup update failed: {error}");
                        let _ = startup_toggle.set_checked(startup::enabled());
                    }
                }
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_dashboard(tray.app_handle());
            }
        })
        .build(app)?;

    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // This must be the first plugin. A shortcut click while the tray app is
        // already running should reveal that window, not start a second poller.
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            if !startup::background_requested(&args) {
                show_dashboard(app);
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .manage(WindowModeState::default())
        .setup(|app| {
            #[cfg(desktop)]
            {
                if let Err(error) = startup::refresh_enabled_path() {
                    eprintln!("launch at startup path refresh failed: {error}");
                }
                build_tray(app.handle())?;
                if let Err(error) = native_host::register() {
                    eprintln!("native host registration failed: {error}");
                }
                if let Err(error) = bridge::install(app.handle()) {
                    eprintln!("browser bridge staging failed: {error}");
                }
                activity::watch(app.handle().clone());

                // Installer finish-page launches and shortcut launches are
                // explicit user actions, so show the dashboard. Only the Run
                // key passes --hidden and starts quietly in the tray.
                if !startup::is_background_launch() {
                    show_dashboard(app.handle());
                }
            }

            // The dashboard fetches for itself on load and on every reveal, so
            // there is no startup fetch here — one would just double the
            // requests made against four undocumented endpoints.

            Ok(())
        })
        // Closing the window returns the deck to the tray instead of exiting —
        // it has to keep polling to be worth having.
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            claude_quota,
            codex_quota,
            gemini_quota,
            grok_quota,
            system_activity,
            bridge_dir,
            reveal_bridge_dir,
            set_mini_mode
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
