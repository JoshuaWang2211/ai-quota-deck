//! Best-effort Windows taskbar overlay for Strip mode.
//!
//! Windows exposes taskbar geometry, but it does not provide a supported way
//! for a normal Tauri window to become part of the Windows 11 taskbar. The
//! Strip therefore remains an independent topmost tool window. While the user
//! opts into this mode, this module keeps it inside the taskbar rectangle and
//! restores its place in the topmost z-order after Explorer raises the taskbar.

use crate::widget::{self, DisplayMode, WidgetPreferences, WidgetState, WINDOW_LABEL};
use std::{mem::size_of, thread, time::Duration};
use tauri::{AppHandle, Manager, PhysicalPosition, WebviewWindow};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, RECT},
    Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST},
    UI::{
        Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON},
        WindowsAndMessaging::{
            EnumWindows, GetClassNameW, GetForegroundWindow, GetWindow, GetWindowRect,
            IsWindowVisible, SetWindowPos, ShowWindowAsync, GW_HWNDPREV, HWND_TOPMOST,
            SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_HIDE, SW_SHOWNOACTIVATE,
        },
    },
};

const ACTIVE_CHECK_INTERVAL: Duration = Duration::from_millis(150);
const INACTIVE_CHECK_INTERVAL: Duration = Duration::from_millis(750);
const TASKBAR_MARGIN: i32 = 4;
const MIN_EXPOSED_TASKBAR_THICKNESS: i32 = 8;
const FULLSCREEN_TOLERANCE: i32 = 2;
const MAX_Z_ORDER_STEPS: usize = 4_096;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ScreenRect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl ScreenRect {
    fn width(self) -> i32 {
        self.right.saturating_sub(self.left)
    }

    fn height(self) -> i32 {
        self.bottom.saturating_sub(self.top)
    }

    fn center(self) -> (i32, i32) {
        (
            self.left.saturating_add(self.width() / 2),
            self.top.saturating_add(self.height() / 2),
        )
    }

    fn intersection(self, other: Self) -> Option<Self> {
        let intersection = Self {
            left: self.left.max(other.left),
            top: self.top.max(other.top),
            right: self.right.min(other.right),
            bottom: self.bottom.min(other.bottom),
        };
        (intersection.width() > 0 && intersection.height() > 0).then_some(intersection)
    }

    fn covers(self, other: Self, tolerance: i32) -> bool {
        self.left <= other.left.saturating_add(tolerance)
            && self.top <= other.top.saturating_add(tolerance)
            && self.right >= other.right.saturating_sub(tolerance)
            && self.bottom >= other.bottom.saturating_sub(tolerance)
    }

    fn distance_squared_to(self, point: (i32, i32)) -> i64 {
        let dx = if point.0 < self.left {
            i64::from(self.left - point.0)
        } else if point.0 > self.right {
            i64::from(point.0 - self.right)
        } else {
            0
        };
        let dy = if point.1 < self.top {
            i64::from(self.top - point.1)
        } else if point.1 > self.bottom {
            i64::from(point.1 - self.bottom)
        } else {
            0
        };
        dx.saturating_mul(dx).saturating_add(dy.saturating_mul(dy))
    }

    fn horizontal(self) -> bool {
        self.width() >= self.height()
    }
}

impl From<RECT> for ScreenRect {
    fn from(value: RECT) -> Self {
        Self {
            left: value.left,
            top: value.top,
            right: value.right,
            bottom: value.bottom,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Taskbar {
    hwnd: HWND,
    bounds: ScreenRect,
    monitor: ScreenRect,
}

fn native_hwnd(window: &WebviewWindow) -> Result<HWND, String> {
    window
        .hwnd()
        .map(|handle| handle.0 as HWND)
        .map_err(|error| error.to_string())
}

fn window_rect(hwnd: HWND) -> Option<ScreenRect> {
    let mut bounds = RECT::default();
    (unsafe { GetWindowRect(hwnd, &mut bounds) } != 0).then(|| bounds.into())
}

fn monitor_rect(hwnd: HWND) -> Option<ScreenRect> {
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    if monitor.is_null() {
        return None;
    }
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    (unsafe { GetMonitorInfoW(monitor, &mut info) } != 0).then(|| info.rcMonitor.into())
}

fn class_name(hwnd: HWND) -> Option<String> {
    let mut buffer = [0_u16; 96];
    let length = unsafe { GetClassNameW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32) };
    (length > 0).then(|| String::from_utf16_lossy(&buffer[..length as usize]))
}

fn is_taskbar_class(class_name: &str) -> bool {
    matches!(class_name, "Shell_TrayWnd" | "Shell_SecondaryTrayWnd")
}

unsafe extern "system" fn collect_taskbars(hwnd: HWND, lparam: LPARAM) -> i32 {
    let Some(class_name) = class_name(hwnd) else {
        return 1;
    };
    if !is_taskbar_class(&class_name) {
        return 1;
    }
    let (Some(bounds), Some(monitor)) = (window_rect(hwnd), monitor_rect(hwnd)) else {
        return 1;
    };
    let taskbars = unsafe { &mut *(lparam as *mut Vec<Taskbar>) };
    taskbars.push(Taskbar {
        hwnd,
        bounds,
        monitor,
    });
    1
}

fn taskbars() -> Vec<Taskbar> {
    let mut taskbars = Vec::new();
    unsafe {
        EnumWindows(
            Some(collect_taskbars),
            (&mut taskbars as *mut Vec<Taskbar>) as LPARAM,
        );
    }
    taskbars
}

fn nearest_taskbar(taskbars: &[Taskbar], window_bounds: ScreenRect) -> Option<&Taskbar> {
    let center = window_bounds.center();
    taskbars
        .iter()
        .min_by_key(|taskbar| taskbar.bounds.distance_squared_to(center))
}

fn snapped_position(window: ScreenRect, taskbar: ScreenRect) -> PhysicalPosition<i32> {
    if taskbar.horizontal() {
        let min_x = taskbar.left.saturating_add(TASKBAR_MARGIN);
        let max_x = taskbar
            .right
            .saturating_sub(window.width())
            .saturating_sub(TASKBAR_MARGIN)
            .max(min_x);
        PhysicalPosition::new(
            window.left.clamp(min_x, max_x),
            taskbar
                .top
                .saturating_add((taskbar.height() - window.height()) / 2),
        )
    } else {
        let min_y = taskbar.top.saturating_add(TASKBAR_MARGIN);
        let max_y = taskbar
            .bottom
            .saturating_sub(window.height())
            .saturating_sub(TASKBAR_MARGIN)
            .max(min_y);
        PhysicalPosition::new(
            taskbar
                .left
                .saturating_add((taskbar.width() - window.width()) / 2),
            window.top.clamp(min_y, max_y),
        )
    }
}

fn has_exposed_thickness(bounds: ScreenRect, monitor: ScreenRect) -> bool {
    let Some(exposed) = bounds.intersection(monitor) else {
        return false;
    };
    if bounds.horizontal() {
        exposed.height() >= MIN_EXPOSED_TASKBAR_THICKNESS
    } else {
        exposed.width() >= MIN_EXPOSED_TASKBAR_THICKNESS
    }
}

fn taskbar_is_exposed(taskbar: &Taskbar) -> bool {
    if unsafe { IsWindowVisible(taskbar.hwnd) } == 0 {
        return false;
    }
    has_exposed_thickness(taskbar.bounds, taskbar.monitor)
}

fn is_desktop_surface(hwnd: HWND) -> bool {
    class_name(hwnd).is_some_and(|name| matches!(name.as_str(), "Progman" | "WorkerW"))
}

fn fullscreen_geometry(bounds: ScreenRect, monitor: ScreenRect, above_taskbar: bool) -> bool {
    above_taskbar && bounds.covers(monitor, FULLSCREEN_TOLERANCE)
}

fn foreground_is_fullscreen(foreground: HWND, own: HWND, taskbars: &[Taskbar]) -> bool {
    if foreground.is_null()
        || foreground == own
        || taskbars.iter().any(|taskbar| taskbar.hwnd == foreground)
        || is_desktop_surface(foreground)
    {
        return false;
    }
    let (Some(bounds), Some(monitor)) = (window_rect(foreground), monitor_rect(foreground)) else {
        return false;
    };
    let Some(taskbar) = taskbars.iter().find(|taskbar| taskbar.monitor == monitor) else {
        return false;
    };
    fullscreen_geometry(bounds, monitor, window_is_above(foreground, taskbar.hwnd))
}

fn window_is_above(upper: HWND, lower: HWND) -> bool {
    let mut candidate = unsafe { GetWindow(lower, GW_HWNDPREV) };
    for _ in 0..MAX_Z_ORDER_STEPS {
        if candidate.is_null() {
            return false;
        }
        if candidate == upper {
            return true;
        }
        candidate = unsafe { GetWindow(candidate, GW_HWNDPREV) };
    }
    false
}

fn left_button_down() -> bool {
    (unsafe { GetAsyncKeyState(i32::from(VK_LBUTTON)) }) < 0
}

fn set_visible_without_activation(hwnd: HWND, visible: bool) {
    unsafe {
        ShowWindowAsync(hwnd, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE });
    }
}

fn raise_without_activation(hwnd: HWND) -> bool {
    (unsafe {
        SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        )
    }) != 0
}

pub fn snap_with_preferences(
    app: &AppHandle,
    state: &WidgetState,
    preferences: &WidgetPreferences,
) -> Result<(), String> {
    if preferences.mode() != DisplayMode::Strip || !preferences.taskbar_overlay {
        return Ok(());
    }
    let window = app
        .get_webview_window(WINDOW_LABEL)
        .ok_or_else(|| "widget window is unavailable".to_string())?;
    let hwnd = native_hwnd(&window)?;
    let bounds = window_rect(hwnd).ok_or_else(|| "cannot read Strip position".to_string())?;
    let taskbars = taskbars();
    let taskbar = nearest_taskbar(&taskbars, bounds)
        .ok_or_else(|| "Windows did not report an available taskbar".to_string())?;
    let position = snapped_position(bounds, taskbar.bounds);
    if (bounds.left, bounds.top) != (position.x, position.y) {
        widget::move_strip_for_taskbar(&window, state, position)?;
    }
    if !raise_without_activation(hwnd) {
        return Err("Windows could not place the Strip above the taskbar".to_string());
    }
    Ok(())
}

pub fn watch(app: AppHandle) {
    thread::spawn(move || {
        let mut hidden_by_overlay = false;
        loop {
            let state = app.state::<WidgetState>();
            let Ok(preferences) = state.snapshot() else {
                thread::sleep(INACTIVE_CHECK_INTERVAL);
                continue;
            };
            let active = preferences.mode() == DisplayMode::Strip && preferences.taskbar_overlay;
            let Some(window) = app.get_webview_window(WINDOW_LABEL) else {
                thread::sleep(INACTIVE_CHECK_INTERVAL);
                continue;
            };
            let Ok(hwnd) = native_hwnd(&window) else {
                thread::sleep(INACTIVE_CHECK_INTERVAL);
                continue;
            };

            if !active {
                if hidden_by_overlay && preferences.visible {
                    set_visible_without_activation(hwnd, true);
                    let _ = raise_without_activation(hwnd);
                }
                hidden_by_overlay = false;
                thread::sleep(INACTIVE_CHECK_INTERVAL);
                continue;
            }

            let Some(bounds) = window_rect(hwnd) else {
                thread::sleep(ACTIVE_CHECK_INTERVAL);
                continue;
            };
            let taskbars = taskbars();
            let Some(taskbar) = nearest_taskbar(&taskbars, bounds) else {
                thread::sleep(ACTIVE_CHECK_INTERVAL);
                continue;
            };
            let foreground = unsafe { GetForegroundWindow() };
            let should_hide = !taskbar_is_exposed(taskbar)
                || foreground_is_fullscreen(foreground, hwnd, &taskbars);

            if should_hide {
                if !hidden_by_overlay {
                    set_visible_without_activation(hwnd, false);
                    hidden_by_overlay = true;
                }
                thread::sleep(ACTIVE_CHECK_INTERVAL);
                continue;
            }

            if hidden_by_overlay {
                set_visible_without_activation(hwnd, true);
                hidden_by_overlay = false;
            }

            if !left_button_down() {
                let position = snapped_position(bounds, taskbar.bounds);
                if (bounds.left, bounds.top) != (position.x, position.y) {
                    let _ = widget::move_strip_for_taskbar(&window, &state, position);
                }
            }

            if window_is_above(taskbar.hwnd, hwnd) {
                let _ = raise_without_activation(hwnd);
            }
            thread::sleep(ACTIVE_CHECK_INTERVAL);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> ScreenRect {
        ScreenRect {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn horizontal_taskbar_keeps_the_strip_inside_and_centred() {
        let position = snapped_position(
            rect(1_700, 1_020, 2_500, 1_060),
            rect(0, 1_040, 1_920, 1_080),
        );
        assert_eq!(position, PhysicalPosition::new(1_116, 1_040));
    }

    #[test]
    fn vertical_taskbar_preserves_the_long_axis() {
        let position = snapped_position(rect(8, 300, 48, 700), rect(0, 0, 60, 1_080));
        assert_eq!(position, PhysicalPosition::new(10, 300));
    }

    #[test]
    fn an_auto_hidden_sliver_is_not_treated_as_an_exposed_taskbar() {
        assert!(!has_exposed_thickness(
            rect(0, 1_078, 1_920, 1_120),
            rect(0, 0, 1_920, 1_080)
        ));
    }

    #[test]
    fn nearest_taskbar_follows_the_strip_to_another_monitor() {
        let taskbars = [
            Taskbar {
                hwnd: std::ptr::null_mut(),
                bounds: rect(0, 1_040, 1_920, 1_080),
                monitor: rect(0, 0, 1_920, 1_080),
            },
            Taskbar {
                hwnd: std::ptr::null_mut(),
                bounds: rect(1_920, 1_400, 4_480, 1_440),
                monitor: rect(1_920, 0, 4_480, 1_440),
            },
        ];
        let selected = nearest_taskbar(&taskbars, rect(2_100, 1_200, 2_900, 1_240)).unwrap();
        assert_eq!(selected.bounds, taskbars[1].bounds);
    }

    #[test]
    fn maximized_window_below_the_taskbar_is_not_fullscreen() {
        let monitor = rect(0, 0, 1_920, 1_080);
        assert!(!fullscreen_geometry(monitor, monitor, false));
    }

    #[test]
    fn window_covering_the_monitor_above_the_taskbar_is_fullscreen() {
        let monitor = rect(0, 0, 1_920, 1_080);
        assert!(fullscreen_geometry(monitor, monitor, true));
    }

    #[test]
    fn ordinary_window_above_the_taskbar_is_not_fullscreen() {
        assert!(!fullscreen_geometry(
            rect(100, 100, 1_600, 900),
            rect(0, 0, 1_920, 1_080),
            true
        ));
    }
}
