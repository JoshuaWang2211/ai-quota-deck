mod activity;
mod antigravity;
mod bridge;
mod claude;
mod claude_rate_limit;
mod codex;
mod gemini;
mod grok;
mod native_host;
mod quota;
mod startup;
#[cfg(target_os = "windows")]
mod taskbar_overlay;
mod widget;

use quota::ProviderQuota;
use tauri::{
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, State, WebviewWindow, WindowEvent,
};

const MAIN_WINDOW: &str = "main";

#[cfg(desktop)]
struct WidgetMenuState {
    widget: CheckMenuItem<tauri::Wry>,
    strip: CheckMenuItem<tauri::Wry>,
    locked: CheckMenuItem<tauri::Wry>,
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
async fn antigravity_quota() -> ProviderQuota {
    antigravity::fetch().await
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
fn widget_preferences(
    state: State<'_, widget::WidgetState>,
) -> Result<widget::WidgetPreferences, String> {
    state.snapshot()
}

#[cfg(desktop)]
fn sync_widget_menu(app: &AppHandle, preferences: &widget::WidgetPreferences) {
    if let Some(menu) = app.try_state::<WidgetMenuState>() {
        let _ = menu
            .widget
            .set_checked(preferences.mode() == widget::DisplayMode::Widget);
        let _ = menu
            .strip
            .set_checked(preferences.mode() == widget::DisplayMode::Strip);
        let _ = menu.locked.set_checked(preferences.locked);
    }
}

#[cfg(not(desktop))]
fn sync_widget_menu(_app: &AppHandle, _preferences: &widget::WidgetPreferences) {}

fn update_display_mode(
    app: &AppHandle,
    state: &widget::WidgetState,
    mode: &str,
) -> Result<widget::WidgetPreferences, String> {
    let preferences = widget::set_mode(app, state, mode)?;
    sync_widget_menu(app, &preferences);
    Ok(preferences)
}

fn switch_display_mode(
    app: &AppHandle,
    state: &widget::WidgetState,
    mode: &str,
) -> Result<widget::WidgetPreferences, String> {
    let preferences = update_display_mode(app, state, mode)?;
    if mode == "dashboard" {
        show_dashboard(app);
    } else if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let _ = window.hide();
    }
    Ok(preferences)
}

fn update_widget_lock(
    app: &AppHandle,
    state: &widget::WidgetState,
    locked: bool,
) -> Result<widget::WidgetPreferences, String> {
    let preferences = widget::set_locked(app, state, locked)?;
    sync_widget_menu(app, &preferences);
    Ok(preferences)
}

#[tauri::command]
fn set_display_mode(
    app: AppHandle,
    state: State<'_, widget::WidgetState>,
    mode: String,
) -> Result<widget::WidgetPreferences, String> {
    switch_display_mode(&app, &state, &mode)
}

#[tauri::command]
fn hide_companion(
    app: AppHandle,
    state: State<'_, widget::WidgetState>,
) -> Result<widget::WidgetPreferences, String> {
    update_display_mode(&app, &state, "dashboard")
}

#[tauri::command]
fn set_widget_locked(
    app: AppHandle,
    state: State<'_, widget::WidgetState>,
    locked: bool,
) -> Result<widget::WidgetPreferences, String> {
    update_widget_lock(&app, &state, locked)
}

#[tauri::command]
fn set_taskbar_overlay(
    app: AppHandle,
    state: State<'_, widget::WidgetState>,
    enabled: bool,
) -> Result<widget::WidgetPreferences, String> {
    widget::set_taskbar_overlay(&app, &state, enabled)
}

#[tauri::command]
fn set_provider_hidden(
    app: AppHandle,
    state: State<'_, widget::WidgetState>,
    id: String,
    hidden: bool,
) -> Result<widget::WidgetPreferences, String> {
    widget::set_provider_hidden(&app, &state, &id, hidden)
}

#[tauri::command]
fn start_widget_drag(
    window: WebviewWindow,
    state: State<'_, widget::WidgetState>,
) -> Result<(), String> {
    widget::start_dragging(&window, &state)
}

#[tauri::command]
fn resize_widget(
    window: WebviewWindow,
    state: State<'_, widget::WidgetState>,
    width: f64,
    height: f64,
) -> Result<(), String> {
    widget::resize(&window, &state, width, height)
}

#[tauri::command]
fn open_dashboard(app: AppHandle, state: State<'_, widget::WidgetState>) {
    if let Err(error) = switch_display_mode(&app, &state, "dashboard") {
        eprintln!("dashboard mode update failed: {error}");
        show_dashboard(&app);
    }
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
    let widget_preferences = app
        .state::<widget::WidgetState>()
        .snapshot()
        .unwrap_or_default();
    let widget_visible = widget_preferences.mode() == widget::DisplayMode::Widget;
    let strip_visible = widget_preferences.mode() == widget::DisplayMode::Strip;
    let show_widget = CheckMenuItem::with_id(
        app,
        "show-widget",
        "Show widget",
        true,
        widget_visible,
        None::<&str>,
    )?;
    let show_strip = CheckMenuItem::with_id(
        app,
        "show-strip",
        "Show strip",
        true,
        strip_visible,
        None::<&str>,
    )?;
    let widget_locked = CheckMenuItem::with_id(
        app,
        "widget-locked",
        "Lock widget position",
        true,
        widget_preferences.locked,
        None::<&str>,
    )?;
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
            &show_widget,
            &show_strip,
            &widget_locked,
            &PredefinedMenuItem::separator(app)?,
            &launch_at_startup,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;
    let startup_toggle = launch_at_startup.clone();
    let _ = app.manage(WidgetMenuState {
        widget: show_widget.clone(),
        strip: show_strip.clone(),
        locked: widget_locked.clone(),
    });

    TrayIconBuilder::with_id("main-tray")
        .icon(app.default_window_icon().unwrap().clone())
        .tooltip("AI Quota Deck")
        .menu(&menu)
        // Left click toggles the dashboard, so the menu belongs on right click only.
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| match event.id.as_ref() {
            "open" => show_dashboard(app),
            "show-widget" => {
                let state = app.state::<widget::WidgetState>();
                if let Ok(preferences) = state.snapshot() {
                    let result = if preferences.mode() == widget::DisplayMode::Widget {
                        update_display_mode(app, &state, "dashboard")
                    } else {
                        switch_display_mode(app, &state, "widget")
                    };
                    if let Err(error) = result {
                        eprintln!("widget mode update failed: {error}");
                    }
                }
            }
            "show-strip" => {
                let state = app.state::<widget::WidgetState>();
                if let Ok(preferences) = state.snapshot() {
                    let result = if preferences.mode() == widget::DisplayMode::Strip {
                        update_display_mode(app, &state, "dashboard")
                    } else {
                        switch_display_mode(app, &state, "strip")
                    };
                    if let Err(error) = result {
                        eprintln!("strip mode update failed: {error}");
                    }
                }
            }
            "widget-locked" => {
                let state = app.state::<widget::WidgetState>();
                if let Ok(preferences) = state.snapshot() {
                    if let Err(error) = update_widget_lock(app, &state, !preferences.locked) {
                        eprintln!("widget lock update failed: {error}");
                    }
                }
            }
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
                let state = app.state::<widget::WidgetState>();
                if let Err(error) = switch_display_mode(app, &state, "dashboard") {
                    eprintln!("dashboard mode update failed: {error}");
                    show_dashboard(app);
                }
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .manage(widget::WidgetState::load())
        .setup(|app| {
            #[cfg(desktop)]
            {
                if let Err(error) = startup::refresh_enabled_path() {
                    eprintln!("launch at startup path refresh failed: {error}");
                }
                build_tray(app.handle())?;
                let widget_state = app.state::<widget::WidgetState>();
                if let Err(error) = widget::apply(app.handle(), &widget_state) {
                    eprintln!("widget initialization failed: {error}");
                }
                if let Err(error) = native_host::register() {
                    eprintln!("native host registration failed: {error}");
                }
                if let Err(error) = bridge::install(app.handle()) {
                    eprintln!("browser bridge staging failed: {error}");
                }
                activity::watch(app.handle().clone());

                #[cfg(target_os = "windows")]
                taskbar_overlay::watch(app.handle().clone());
                // Installer finish-page launches and shortcut launches are
                // explicit user actions, so show the dashboard. Only the Run
                // key passes --hidden and starts quietly in the tray.
                if !startup::is_background_launch() {
                    if let Err(error) =
                        switch_display_mode(app.handle(), &widget_state, "dashboard")
                    {
                        eprintln!("dashboard mode update failed: {error}");
                        show_dashboard(app.handle());
                    }
                }
            }

            // The dashboard fetches for itself on load and on every reveal, so
            // there is no startup fetch here — one would just double the
            // requests made against five undocumented endpoints.

            Ok(())
        })
        // Closing the window returns the deck to the tray instead of exiting —
        // it has to keep polling to be worth having.
        .on_window_event(|window, event| match event {
            WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                if window.label() == widget::WINDOW_LABEL {
                    let state = window.state::<widget::WidgetState>();
                    if let Err(error) =
                        update_display_mode(window.app_handle(), &state, "dashboard")
                    {
                        eprintln!("widget close failed: {error}");
                    }
                } else {
                    let _ = window.hide();
                }
            }
            WindowEvent::Moved(position) if window.label() == widget::WINDOW_LABEL => {
                let state = window.state::<widget::WidgetState>();
                if let Err(error) = state.remember_position(*position) {
                    eprintln!("widget position save failed: {error}");
                }
            }
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            claude_quota,
            codex_quota,
            gemini_quota,
            grok_quota,
            antigravity_quota,
            system_activity,
            bridge_dir,
            reveal_bridge_dir,
            widget_preferences,
            set_display_mode,
            hide_companion,
            set_widget_locked,
            start_widget_drag,
            resize_widget,
            set_taskbar_overlay,
            set_provider_hidden,
            open_dashboard
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
