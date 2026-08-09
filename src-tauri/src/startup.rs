use std::path::Path;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "AI Quota Deck";

fn command_for(executable: &Path) -> String {
    format!("\"{}\" --hidden", executable.display())
}

pub fn background_requested(args: &[String]) -> bool {
    args.iter().any(|argument| argument == "--hidden")
}

pub fn is_background_launch() -> bool {
    std::env::args().any(|argument| argument == "--hidden")
}

#[cfg(windows)]
pub fn enabled() -> bool {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(RUN_KEY)
        .and_then(|key| key.get_value::<String, _>(VALUE_NAME))
        .is_ok()
}

#[cfg(not(windows))]
pub fn enabled() -> bool {
    false
}

#[cfg(windows)]
pub fn set_enabled(enable: bool) -> Result<(), String> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (key, _) = hkcu
        .create_subkey(RUN_KEY)
        .map_err(|error| format!("cannot open HKCU\\{RUN_KEY}: {error}"))?;

    if enable {
        let executable = std::env::current_exe()
            .map_err(|error| format!("cannot locate the running executable: {error}"))?;
        key.set_value(VALUE_NAME, &command_for(&executable))
            .map_err(|error| format!("cannot enable launch at startup: {error}"))
    } else {
        match key.delete_value(VALUE_NAME) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("cannot disable launch at startup: {error}")),
        }
    }
}

#[cfg(not(windows))]
pub fn set_enabled(_enable: bool) -> Result<(), String> {
    Err("launch at startup is currently Windows-only".to_string())
}

/// Keep an enabled entry pointed at the current installed executable after an
/// app update or path move. A disabled entry stays absent.
pub fn refresh_enabled_path() -> Result<(), String> {
    if enabled() {
        set_enabled(true)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_paths_for_the_windows_run_key() {
        assert_eq!(
            command_for(Path::new(
                r"C:\Program Files\AI Quota Deck\ai-quota-deck.exe"
            )),
            r#""C:\Program Files\AI Quota Deck\ai-quota-deck.exe" --hidden"#
        );
    }

    #[test]
    fn only_the_hidden_flag_requests_a_background_launch() {
        assert!(background_requested(&[
            "ai-quota-deck.exe".to_string(),
            "--hidden".to_string(),
        ]));
        assert!(!background_requested(&["ai-quota-deck.exe".to_string()]));
    }
}
