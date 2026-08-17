use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex, MutexGuard,
    },
    time::{Duration, Instant},
};
use tauri::{
    AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, PhysicalSize, WebviewWindow,
};

pub const WINDOW_LABEL: &str = "widget";
const DEFAULT_WIDGET_WIDTH: f64 = 244.0;
const DEFAULT_WIDGET_HEIGHT: f64 = 164.0;
const MIN_WIDGET_WIDTH: f64 = 200.0;
const MAX_WIDGET_WIDTH: f64 = 360.0;
const MIN_WIDGET_HEIGHT: f64 = 72.0;
const MAX_WIDGET_HEIGHT: f64 = 260.0;
const DEFAULT_STRIP_WIDTH: f64 = 560.0;
const MIN_STRIP_WIDTH: f64 = 300.0;
const MAX_STRIP_WIDTH: f64 = 900.0;
const STRIP_HEIGHT: f64 = 40.0;
const SCREEN_MARGIN: i32 = 24;
const USER_DRAG_WINDOW: Duration = Duration::from_secs(30);
const PROGRAMMATIC_MOVE_WINDOW: Duration = Duration::from_secs(5);
const PREFERENCES_EVENT: &str = "widget-preferences-changed";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DisplayMode {
    #[default]
    Dashboard,
    Widget,
    Strip,
}

impl DisplayMode {
    pub fn parse(mode: &str) -> Result<Self, String> {
        match mode {
            "dashboard" => Ok(Self::Dashboard),
            "widget" => Ok(Self::Widget),
            "strip" => Ok(Self::Strip),
            _ => Err(format!("unknown display mode: {mode}")),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct WidgetPreferences {
    pub visible: bool,
    pub locked: bool,
    pub strip: bool,
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub strip_x: Option<i32>,
    pub strip_y: Option<i32>,
    pub taskbar_overlay: bool,
}

impl WidgetPreferences {
    pub fn mode(&self) -> DisplayMode {
        match (self.visible, self.strip) {
            (true, true) => DisplayMode::Strip,
            (true, false) => DisplayMode::Widget,
            (false, _) => DisplayMode::Dashboard,
        }
    }

    fn apply_mode(&mut self, mode: DisplayMode) {
        match mode {
            DisplayMode::Dashboard => {
                self.visible = false;
                self.strip = false;
            }
            DisplayMode::Widget => {
                self.visible = true;
                self.strip = false;
            }
            DisplayMode::Strip => {
                self.visible = true;
                self.strip = true;
            }
        }
    }
}

pub struct WidgetState {
    preferences: Mutex<WidgetPreferences>,
    position_updates_suspended: AtomicBool,
    user_drag_until: Mutex<Option<Instant>>,
    programmatic_position: Mutex<Option<(PhysicalPosition<i32>, Instant)>>,
}

impl WidgetState {
    pub fn load() -> Self {
        let preferences = preferences_path()
            .and_then(|path| fs::read_to_string(path).ok())
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        Self {
            preferences: Mutex::new(preferences),
            position_updates_suspended: AtomicBool::new(false),
            user_drag_until: Mutex::new(None),
            programmatic_position: Mutex::new(None),
        }
    }

    pub fn snapshot(&self) -> Result<WidgetPreferences, String> {
        Ok(self.lock()?.clone())
    }

    fn lock(&self) -> Result<MutexGuard<'_, WidgetPreferences>, String> {
        self.preferences
            .lock()
            .map_err(|_| "widget preferences are unavailable".to_string())
    }

    fn replace(&self, preferences: WidgetPreferences) -> Result<WidgetPreferences, String> {
        persist(&preferences)?;
        *self.lock()? = preferences.clone();
        Ok(preferences)
    }

    fn suspend_position_updates(&self, suspended: bool) {
        self.position_updates_suspended
            .store(suspended, Ordering::Release);
    }

    fn begin_user_drag(&self) -> Result<(), String> {
        *self
            .user_drag_until
            .lock()
            .map_err(|_| "widget drag state is unavailable".to_string())? =
            Some(Instant::now() + USER_DRAG_WINDOW);
        Ok(())
    }

    fn end_user_drag(&self) {
        if let Ok(mut until) = self.user_drag_until.lock() {
            *until = None;
        }
    }

    fn user_drag_active(&self) -> bool {
        let Ok(mut until) = self.user_drag_until.lock() else {
            return false;
        };
        if until.is_some_and(|deadline| deadline > Instant::now()) {
            return true;
        }
        *until = None;
        false
    }

    fn expect_programmatic_position(&self, position: PhysicalPosition<i32>) {
        if let Ok(mut expected) = self.programmatic_position.lock() {
            *expected = Some((position, Instant::now() + PROGRAMMATIC_MOVE_WINDOW));
        }
    }

    fn is_programmatic_position(&self, position: PhysicalPosition<i32>) -> bool {
        let Ok(mut expected) = self.programmatic_position.lock() else {
            return false;
        };
        let matches = expected.is_some_and(|(candidate, deadline)| {
            deadline > Instant::now() && candidate == position
        });
        if matches || expected.is_some_and(|(_, deadline)| deadline <= Instant::now()) {
            *expected = None;
        }
        matches
    }

    pub fn remember_position(&self, position: PhysicalPosition<i32>) -> Result<(), String> {
        if self.position_updates_suspended.load(Ordering::Acquire) {
            return Ok(());
        }
        if self.is_programmatic_position(position) || !self.user_drag_active() {
            return Ok(());
        }
        let mut preferences = self.snapshot()?;
        if !store_position(&mut preferences, position) {
            return Ok(());
        }
        self.replace(preferences).map(|_| ())
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ScreenRect {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

impl ScreenRect {
    fn from_monitor(monitor: &tauri::Monitor) -> Self {
        Self {
            x: monitor.position().x,
            y: monitor.position().y,
            width: monitor.size().width,
            height: monitor.size().height,
        }
    }

    fn contains(self, x: i32, y: i32) -> bool {
        x >= self.x
            && i64::from(x) < i64::from(self.x) + i64::from(self.width)
            && y >= self.y
            && i64::from(y) < i64::from(self.y) + i64::from(self.height)
    }
}

fn preferences_path() -> Option<PathBuf> {
    dirs::data_local_dir().map(|dir| dir.join("ai-quota-deck").join("widget.json"))
}

fn persist(preferences: &WidgetPreferences) -> Result<(), String> {
    let path = preferences_path()
        .ok_or_else(|| "could not locate the local application data directory".to_string())?;
    let parent = path
        .parent()
        .ok_or_else(|| "widget preferences path has no parent directory".to_string())?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create widget preferences directory: {error}"))?;
    let json = serde_json::to_string_pretty(preferences)
        .map_err(|error| format!("cannot serialize widget preferences: {error}"))?;
    fs::write(path, json).map_err(|error| format!("cannot save widget preferences: {error}"))
}

fn screens(window: &WebviewWindow) -> Result<(Vec<ScreenRect>, Option<ScreenRect>), String> {
    let monitors = window
        .available_monitors()
        .map_err(|error| error.to_string())?;
    let screens = monitors.iter().map(ScreenRect::from_monitor).collect();
    let primary = window
        .primary_monitor()
        .map_err(|error| error.to_string())?
        .as_ref()
        .map(ScreenRect::from_monitor);
    Ok((screens, primary))
}

fn saved_position(preferences: &WidgetPreferences) -> Option<PhysicalPosition<i32>> {
    let coordinates = if preferences.strip {
        preferences.strip_x.zip(preferences.strip_y)
    } else {
        preferences.x.zip(preferences.y)
    };
    coordinates.map(|(x, y)| PhysicalPosition::new(x, y))
}

fn should_place_after_resize(preferences: &WidgetPreferences) -> bool {
    saved_position(preferences).is_none()
}

fn store_position(preferences: &mut WidgetPreferences, position: PhysicalPosition<i32>) -> bool {
    let current = if preferences.strip {
        preferences.strip_x.zip(preferences.strip_y)
    } else {
        preferences.x.zip(preferences.y)
    };
    if current == Some((position.x, position.y)) {
        return false;
    }
    if preferences.strip {
        preferences.strip_x = Some(position.x);
        preferences.strip_y = Some(position.y);
    } else {
        preferences.x = Some(position.x);
        preferences.y = Some(position.y);
    }
    true
}

fn clamped_position(
    saved: Option<PhysicalPosition<i32>>,
    window_size: PhysicalSize<u32>,
    screens: &[ScreenRect],
    primary: Option<ScreenRect>,
) -> Option<PhysicalPosition<i32>> {
    if let Some(saved) = saved {
        if let Some(screen) = screens
            .iter()
            .find(|screen| screen.contains(saved.x, saved.y))
        {
            let max_x =
                i64::from(screen.x) + i64::from(screen.width) - i64::from(window_size.width);
            let max_y =
                i64::from(screen.y) + i64::from(screen.height) - i64::from(window_size.height);
            return Some(PhysicalPosition::new(
                i64::from(saved.x).clamp(i64::from(screen.x), max_x.max(i64::from(screen.x)))
                    as i32,
                i64::from(saved.y).clamp(i64::from(screen.y), max_y.max(i64::from(screen.y)))
                    as i32,
            ));
        }
    }

    let screen = primary.or_else(|| screens.first().copied())?;
    let x = i64::from(screen.x) + i64::from(screen.width)
        - i64::from(window_size.width)
        - i64::from(SCREEN_MARGIN);
    Some(PhysicalPosition::new(
        x.max(i64::from(screen.x)) as i32,
        screen.y.saturating_add(SCREEN_MARGIN),
    ))
}

fn place_on_screen(
    window: &WebviewWindow,
    preferences: &WidgetPreferences,
) -> Result<PhysicalPosition<i32>, String> {
    let window_size = window.outer_size().map_err(|error| error.to_string())?;
    let (screens, primary) = screens(window)?;
    clamped_position(saved_position(preferences), window_size, &screens, primary)
        .ok_or_else(|| "Windows did not report an available monitor".to_string())
}

fn emit_preferences(app: &AppHandle, preferences: &WidgetPreferences) {
    let _ = app.emit(PREFERENCES_EVENT, preferences);
}

fn prepare_document(window: &WebviewWindow, preferences: &WidgetPreferences) {
    let mode = match preferences.mode() {
        DisplayMode::Strip => "strip",
        _ => "widget",
    };
    let _ = window.eval(format!(
        "document.documentElement.dataset.mode='{mode}';document.documentElement.dataset.locked='{}';document.documentElement.dataset.taskbarOverlay='{}';",
        preferences.locked,
        preferences.taskbar_overlay
    ));
}

fn initial_size(preferences: &WidgetPreferences) -> LogicalSize<f64> {
    if preferences.mode() == DisplayMode::Strip {
        LogicalSize::new(DEFAULT_STRIP_WIDTH, STRIP_HEIGHT)
    } else {
        LogicalSize::new(DEFAULT_WIDGET_WIDTH, DEFAULT_WIDGET_HEIGHT)
    }
}

pub fn apply(app: &AppHandle, state: &WidgetState) -> Result<WidgetPreferences, String> {
    let window = app
        .get_webview_window(WINDOW_LABEL)
        .ok_or_else(|| "widget window is unavailable".to_string())?;
    let preferences = state.snapshot()?;
    if preferences.visible {
        prepare_document(&window, &preferences);
    }
    state.end_user_drag();
    state.suspend_position_updates(true);
    let result = (|| -> Result<(), String> {
        if !preferences.visible {
            return window.hide().map_err(|error| error.to_string());
        }
        window.hide().map_err(|error| error.to_string())?;
        window
            .set_size(initial_size(&preferences))
            .map_err(|error| error.to_string())?;
        let position = place_on_screen(&window, &preferences)?;
        state.expect_programmatic_position(position);
        window
            .set_position(position)
            .map_err(|error| error.to_string())?;
        window.show().map_err(|error| error.to_string())
    })();
    state.suspend_position_updates(false);
    result?;
    let preferences = state.replace(preferences)?;
    emit_preferences(app, &preferences);
    Ok(preferences)
}

pub fn set_mode(
    app: &AppHandle,
    state: &WidgetState,
    mode: &str,
) -> Result<WidgetPreferences, String> {
    let previous = state.snapshot()?;
    let next = preferences_for_mode(previous.clone(), mode)?;
    state.replace(next)?;
    match apply(app, state) {
        Ok(preferences) => Ok(preferences),
        Err(error) => {
            let _ = state.replace(previous);
            let _ = apply(app, state);
            Err(error)
        }
    }
}

fn preferences_for_mode(
    mut preferences: WidgetPreferences,
    mode: &str,
) -> Result<WidgetPreferences, String> {
    preferences.apply_mode(DisplayMode::parse(mode)?);
    Ok(preferences)
}

pub fn set_locked(
    app: &AppHandle,
    state: &WidgetState,
    locked: bool,
) -> Result<WidgetPreferences, String> {
    let mut preferences = state.snapshot()?;
    preferences.locked = locked;
    let preferences = state.replace(preferences)?;
    emit_preferences(app, &preferences);
    Ok(preferences)
}

#[cfg_attr(not(target_os = "windows"), allow(unused_variables))]
pub fn set_taskbar_overlay(
    app: &AppHandle,
    state: &WidgetState,
    enabled: bool,
) -> Result<WidgetPreferences, String> {
    let previous = state.snapshot()?;
    if previous.taskbar_overlay == enabled {
        return Ok(previous);
    }

    #[cfg(not(target_os = "windows"))]
    if enabled {
        return Err("taskbar overlay is available only on Windows".to_string());
    }

    let mut next = previous.clone();
    next.taskbar_overlay = enabled;

    #[cfg(target_os = "windows")]
    if enabled && next.mode() == DisplayMode::Strip {
        crate::taskbar_overlay::snap_with_preferences(app, state, &next)?;
    }

    let preferences = state.replace(next)?;
    emit_preferences(app, &preferences);
    Ok(preferences)
}

pub fn start_dragging(window: &WebviewWindow, state: &WidgetState) -> Result<(), String> {
    if window.label() != WINDOW_LABEL {
        return Err("only the companion window can use the drag command".to_string());
    }
    let preferences = state.snapshot()?;
    if !preferences.strip && preferences.locked {
        return Err("unlock the widget before moving it".to_string());
    }
    state.begin_user_drag()?;
    if let Err(error) = window.start_dragging() {
        state.end_user_drag();
        return Err(error.to_string());
    }
    Ok(())
}

pub fn resize(
    window: &WebviewWindow,
    state: &WidgetState,
    width: f64,
    height: f64,
) -> Result<(), String> {
    if window.label() != WINDOW_LABEL {
        return Err("only the companion window can resize itself".to_string());
    }
    let preferences = state.snapshot()?;
    let size = if preferences.strip {
        let width = if width.is_finite() {
            width.clamp(MIN_STRIP_WIDTH, MAX_STRIP_WIDTH)
        } else {
            DEFAULT_STRIP_WIDTH
        };
        LogicalSize::new(width, STRIP_HEIGHT)
    } else {
        let width = if width.is_finite() {
            width.clamp(MIN_WIDGET_WIDTH, MAX_WIDGET_WIDTH)
        } else {
            DEFAULT_WIDGET_WIDTH
        };
        let height = if height.is_finite() {
            height.clamp(MIN_WIDGET_HEIGHT, MAX_WIDGET_HEIGHT)
        } else {
            DEFAULT_WIDGET_HEIGHT
        };
        LogicalSize::new(width, height)
    };

    state.suspend_position_updates(true);
    let result = (|| -> Result<(), String> {
        window.set_size(size).map_err(|error| error.to_string())?;
        // Once the user has placed a companion, content-driven resizing must
        // never touch its coordinates. Re-running monitor selection here made
        // mixed-DPI physical positions look off-screen and snapped a correctly
        // saved Strip back to the default top-right position a few seconds
        // after a drag. A never-positioned companion still gets its initial
        // top-right anchor after the measured size is known.
        if state.user_drag_active() || !should_place_after_resize(&preferences) {
            return Ok(());
        }
        let position = place_on_screen(window, &preferences)?;
        state.expect_programmatic_position(position);
        window
            .set_position(position)
            .map_err(|error| error.to_string())?;
        Ok(())
    })();
    state.suspend_position_updates(false);
    result
}

pub(crate) fn move_strip_for_taskbar(
    window: &WebviewWindow,
    state: &WidgetState,
    position: PhysicalPosition<i32>,
) -> Result<(), String> {
    if window.label() != WINDOW_LABEL {
        return Err("only the companion window can use taskbar placement".to_string());
    }

    // Overlay alignment is not a user drag. Move the window and do not persist
    // — strip_x/strip_y stay the last place the user put it. After restart the
    // watcher snaps again from that saved long-axis coordinate.
    state.expect_programmatic_position(position);
    state.suspend_position_updates(true);
    let result = window
        .set_position(position)
        .map_err(|error| error.to_string());
    state.suspend_position_updates(false);
    result
}
/// Reassert a companion window after Windows wakes and restores its monitor
/// topology. Windows can temporarily relocate always-on-top windows while an
/// external display is still waking; that system move must not replace the
/// user's saved position.
pub fn restore_after_resume(app: &AppHandle, state: &WidgetState) -> Result<bool, String> {
    let preferences = state.snapshot()?;
    if !preferences.visible || state.user_drag_active() {
        return Ok(false);
    }
    let window = app
        .get_webview_window(WINDOW_LABEL)
        .ok_or_else(|| "widget window is unavailable".to_string())?;

    window
        .set_always_on_top(true)
        .map_err(|error| error.to_string())?;
    if !window.is_visible().unwrap_or(false) {
        window.show().map_err(|error| error.to_string())?;
    }

    let Some(saved) = saved_position(&preferences) else {
        return Ok(true);
    };
    let window_size = window.outer_size().map_err(|error| error.to_string())?;
    let (screens, _) = screens(&window)?;
    // Do not fall back to the primary display while a saved monitor is still
    // waking. A later retry will restore the original screen; a permanent
    // disconnection is handled by normal mode activation without erasing it.
    if !screens
        .iter()
        .any(|screen| screen.contains(saved.x, saved.y))
    {
        return Ok(false);
    }
    let Some(position) = clamped_position(Some(saved), window_size, &screens, None) else {
        return Ok(false);
    };

    state.suspend_position_updates(true);
    state.expect_programmatic_position(position);
    let result = window
        .set_position(position)
        .map_err(|error| error.to_string());
    state.suspend_position_updates(false);
    result.map(|_| true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(x: i32, y: i32, width: u32, height: u32) -> ScreenRect {
        ScreenRect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn new_companion_is_opt_in_unlocked_and_has_independent_positions() {
        let preferences = WidgetPreferences::default();
        assert!(!preferences.visible);
        assert!(!preferences.locked);
        assert!(!preferences.strip);
        assert_eq!(preferences.x, None);
        assert_eq!(preferences.strip_x, None);
    }

    #[test]
    fn saved_position_is_clamped_to_its_monitor() {
        let screens = [screen(100, 50, 1_000, 700)];
        assert_eq!(
            clamped_position(
                Some(PhysicalPosition::new(1_050, 720)),
                PhysicalSize::new(244, 164),
                &screens,
                None,
            ),
            Some(PhysicalPosition::new(856, 586))
        );
    }

    #[test]
    fn missing_or_disconnected_position_uses_primary_top_right() {
        let primary = screen(-1_920, 0, 1_920, 1_080);
        assert_eq!(
            clamped_position(
                Some(PhysicalPosition::new(4_000, 4_000)),
                PhysicalSize::new(244, 164),
                &[primary],
                Some(primary),
            ),
            Some(PhysicalPosition::new(-268, 24))
        );
    }

    #[test]
    fn widget_and_strip_positions_are_kept_separately() {
        let mut preferences = WidgetPreferences::default();
        assert!(store_position(
            &mut preferences,
            PhysicalPosition::new(100, 80)
        ));
        preferences.strip = true;
        assert_eq!(saved_position(&preferences), None);
        assert!(store_position(
            &mut preferences,
            PhysicalPosition::new(500, 300)
        ));
        assert_eq!(
            saved_position(&preferences),
            Some(PhysicalPosition::new(500, 300))
        );
        preferences.strip = false;
        assert_eq!(
            saved_position(&preferences),
            Some(PhysicalPosition::new(100, 80))
        );
    }

    #[test]
    fn content_resize_never_repositions_a_user_placed_companion() {
        let mut preferences = WidgetPreferences::default();
        assert!(should_place_after_resize(&preferences));

        preferences.strip = true;
        preferences.strip_x = Some(9);
        preferences.strip_y = Some(2_073);
        assert!(!should_place_after_resize(&preferences));

        preferences.strip = false;
        preferences.x = Some(285);
        preferences.y = Some(1_789);
        assert!(!should_place_after_resize(&preferences));
    }

    #[test]
    fn display_modes_are_mutually_exclusive_and_preserve_companion_details() {
        let original = WidgetPreferences {
            visible: true,
            locked: true,
            strip: true,
            x: Some(120),
            y: Some(80),
            strip_x: Some(500),
            strip_y: Some(300),
            taskbar_overlay: true,
        };
        let dashboard = preferences_for_mode(original.clone(), "dashboard").unwrap();
        assert!(!dashboard.visible);
        assert!(!dashboard.strip);
        assert!(dashboard.locked);
        assert_eq!(dashboard.x, Some(120));
        assert_eq!(dashboard.strip_x, Some(500));
        assert!(dashboard.taskbar_overlay);

        let widget = preferences_for_mode(original.clone(), "widget").unwrap();
        assert!(widget.visible);
        assert!(!widget.strip);

        let strip = preferences_for_mode(original, "strip").unwrap();
        assert!(strip.visible);
        assert!(strip.strip);
        assert!(preferences_for_mode(strip, "unknown").is_err());
    }

    #[test]
    fn display_mode_is_the_visible_strip_pair() {
        let mut preferences = WidgetPreferences::default();
        assert_eq!(preferences.mode(), DisplayMode::Dashboard);
        preferences.apply_mode(DisplayMode::Widget);
        assert_eq!(preferences.mode(), DisplayMode::Widget);
        assert!(preferences.visible);
        assert!(!preferences.strip);
        preferences.apply_mode(DisplayMode::Strip);
        assert_eq!(preferences.mode(), DisplayMode::Strip);
        preferences.apply_mode(DisplayMode::Dashboard);
        assert_eq!(preferences.mode(), DisplayMode::Dashboard);
        assert!(!preferences.visible);
        assert!(!preferences.strip);
    }

    #[test]
    fn older_preferences_default_taskbar_overlay_to_off() {
        let preferences: WidgetPreferences =
            serde_json::from_str(r#"{"visible":true,"strip":true,"strip_x":8,"strip_y":1040}"#)
                .unwrap();
        assert!(preferences.visible);
        assert!(preferences.strip);
        assert!(!preferences.taskbar_overlay);
    }
}
