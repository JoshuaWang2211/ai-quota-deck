//! Local Windows activity state used to pause provider polling while nobody is
//! at the computer. No input contents are read or stored: Windows exposes only
//! the elapsed time since the last input and whether the input desktop is open.

use serde::Serialize;
use std::time::Duration;
use tauri::Emitter;

pub const IDLE_PAUSE_SECONDS: u64 = 5 * 60;
const ACTIVITY_PROBE_SECONDS: u64 = 5;

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

/// Watch activity outside the WebView so Chromium background throttling cannot
/// delay the transition that wakes Claude polling. The network request itself
/// remains in the normal provider scheduler and re-checks this state first.
pub fn watch(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        let mut was_away = snapshot().user_away();
        loop {
            std::thread::sleep(Duration::from_secs(ACTIVITY_PROBE_SECONDS));
            let user_away = snapshot().user_away();
            if was_away && !user_away {
                let _ = app.emit("system-activity-resumed", ());
            }
            was_away = user_away;
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
}

#[cfg(not(target_os = "windows"))]
pub fn snapshot() -> SystemActivity {
    SystemActivity {
        idle_seconds: 0,
        workstation_locked: false,
    }
}
