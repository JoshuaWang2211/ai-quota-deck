//! Local Windows activity state used to pause provider polling while nobody is
//! at the computer. No input contents are read or stored: Windows exposes only
//! the elapsed time since the last input and whether the input desktop is open.

use serde::Serialize;
use std::time::Duration;
use tauri::{Emitter, Manager};

pub const IDLE_PAUSE_SECONDS: u64 = 5 * 60;
const ACTIVITY_PROBE_SECONDS: u64 = 5;
const ACTIVE_REFRESH_TICK_SECONDS: u64 = 60;

#[derive(Serialize)]
pub struct SystemActivity {
    pub idle_seconds: u64,
    pub workstation_locked: bool,
}

impl SystemActivity {
    pub fn user_away(&self) -> bool {
        self.workstation_locked || self.idle_seconds >= IDLE_PAUSE_SECONDS
    }
}

fn advance_refresh_tick(remaining: u64, user_away: bool, just_resumed: bool) -> (u64, bool) {
    if user_away || just_resumed {
        return (ACTIVE_REFRESH_TICK_SECONDS, false);
    }
    if remaining <= ACTIVITY_PROBE_SECONDS {
        (ACTIVE_REFRESH_TICK_SECONDS, true)
    } else {
        (remaining - ACTIVITY_PROBE_SECONDS, false)
    }
}

/// Drive Claude recovery outside the WebView so Chromium background throttling
/// cannot delay either the post-resume grace period or an expired 429 cooldown.
/// The frontend still owns request floors/backoff and re-checks activity first.
pub fn watch(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        let mut was_away = snapshot().user_away();
        let mut refresh_tick_remaining = ACTIVE_REFRESH_TICK_SECONDS;
        loop {
            std::thread::sleep(Duration::from_secs(ACTIVITY_PROBE_SECONDS));
            let user_away = snapshot().user_away();
            let just_resumed = was_away && !user_away;
            if just_resumed {
                let _ = app.emit("system-activity-resumed", ());
                restore_companion_after_resume(app.clone());
            }
            let (next_remaining, refresh_due) =
                advance_refresh_tick(refresh_tick_remaining, user_away, just_resumed);
            refresh_tick_remaining = next_remaining;
            if refresh_due {
                let _ = app.emit("active-refresh-tick", ());
            }
            was_away = user_away;
        }
    });
}

fn restore_companion_after_resume(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        // Monitor enumeration often changes more than once while a laptop lid
        // or external display wakes. Reapply the saved position after each
        // settling window; the restore itself is a no-op while the user drags.
        for delay in [2, 6, 12] {
            std::thread::sleep(Duration::from_secs(delay));
            let state = app.state::<crate::widget::WidgetState>();
            let _ = crate::widget::restore_after_resume(&app, &state);
        }
    });
}

#[cfg(target_os = "windows")]
mod windows {
    use super::SystemActivity;
    use std::ffi::c_void;

    #[repr(C)]
    struct LastInputInfo {
        cb_size: u32,
        time: u32,
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetLastInputInfo(info: *mut LastInputInfo) -> i32;
        fn OpenInputDesktop(flags: u32, inherit: i32, desired_access: u32) -> *mut c_void;
        fn CloseDesktop(desktop: *mut c_void) -> i32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetTickCount() -> u32;
    }

    fn elapsed_millis(now: u32, last_input: u32) -> u32 {
        // GetTickCount wraps after roughly 49 days. Unsigned subtraction keeps
        // the idle duration correct across the wrap, matching the Win32 clocks.
        now.wrapping_sub(last_input)
    }

    fn idle_seconds() -> u64 {
        let mut info = LastInputInfo {
            cb_size: std::mem::size_of::<LastInputInfo>() as u32,
            time: 0,
        };
        // SAFETY: `info` is the documented fixed-layout LASTINPUTINFO structure
        // and remains valid for the duration of the call.
        if unsafe { GetLastInputInfo(&mut info) } == 0 {
            return 0;
        }
        // SAFETY: GetTickCount has no parameters or caller-owned memory.
        let now = unsafe { GetTickCount() };
        u64::from(elapsed_millis(now, info.time)) / 1000
    }

    fn workstation_locked() -> bool {
        // OpenInputDesktop returns null while the secure lock desktop is active.
        // SAFETY: zero flags/access are sufficient for this availability probe;
        // any non-null handle is closed immediately below.
        let desktop = unsafe { OpenInputDesktop(0, 0, 0) };
        if desktop.is_null() {
            return true;
        }
        // SAFETY: `desktop` is the live handle returned immediately above.
        let _ = unsafe { CloseDesktop(desktop) };
        false
    }

    pub fn snapshot() -> SystemActivity {
        SystemActivity {
            idle_seconds: idle_seconds(),
            workstation_locked: workstation_locked(),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::elapsed_millis;

        #[test]
        fn idle_clock_handles_normal_and_wrapped_ticks() {
            assert_eq!(elapsed_millis(15_000, 10_000), 5_000);
            assert_eq!(elapsed_millis(2_000, u32::MAX - 999), 3_000);
        }
    }
}

#[cfg(target_os = "windows")]
pub fn snapshot() -> SystemActivity {
    windows::snapshot()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn away_state_combines_idle_threshold_and_lock() {
        assert!(!SystemActivity {
            idle_seconds: IDLE_PAUSE_SECONDS - 1,
            workstation_locked: false,
        }
        .user_away());
        assert!(SystemActivity {
            idle_seconds: IDLE_PAUSE_SECONDS,
            workstation_locked: false,
        }
        .user_away());
        assert!(SystemActivity {
            idle_seconds: 0,
            workstation_locked: true,
        }
        .user_away());
    }

    #[test]
    fn native_refresh_tick_waits_after_resume_and_stops_while_away() {
        assert_eq!(advance_refresh_tick(5, false, false), (60, true));
        assert_eq!(advance_refresh_tick(5, true, false), (60, false));
        assert_eq!(advance_refresh_tick(5, false, true), (60, false));
        assert_eq!(advance_refresh_tick(60, false, false), (55, false));
    }
}

#[cfg(not(target_os = "windows"))]
pub fn snapshot() -> SystemActivity {
    SystemActivity {
        idle_seconds: 0,
        workstation_locked: false,
    }
}
