use std::fs;
use std::path::{Path, PathBuf};

use tauri::{AppHandle, Manager};

/// The bundled companion extension files, relative to the `browser-bridge`
/// directory. Listed explicitly rather than walked, so the dev-only
/// `package.json` and `tests/` stay out of what the user loads into Chrome.
const FILES: [&str; 7] = [
    "manifest.json",
    "src/background.js",
    "src/gemini-interceptor.js",
    "src/gemini-parser.js",
    "src/gemini.js",
    "src/grok-parser.js",
    "src/grok.js",
];

const STAMP: &str = ".installed-version";

/// Where the user loads the unpacked extension from. Fixed, so the README can
/// name one literal path: the MSI and NSIS bundles install the app itself to
/// different roots, and the NSIS installer lets the user change its root again.
pub fn install_dir() -> Result<PathBuf, String> {
    dirs::data_local_dir()
        .map(|dir| dir.join("ai-quota-deck").join("browser-bridge"))
        .ok_or_else(|| "could not locate the local application data directory".to_string())
}

/// Copies the bundled bridge to `install_dir` when the stamped version does not
/// match this build. Chrome re-reads an unpacked extension from disk when the
/// browser restarts, so an app update becomes a bridge update without the user
/// touching `chrome://extensions` again.
pub fn install(app: &AppHandle) -> Result<PathBuf, String> {
    let source = app
        .path()
        .resource_dir()
        .map_err(|error| format!("cannot locate the bundled resources: {error}"))?
        .join("browser-bridge");
    let target = install_dir()?;
    sync(&source, &target, env!("CARGO_PKG_VERSION"))?;
    Ok(target)
}

/// Opens the staged bridge folder in Explorer. Done in Rust rather than through
/// the opener plugin's JS binding so the setup panel does not depend on which
/// globals `withGlobalTauri` happens to expose.
#[cfg(windows)]
pub fn reveal() -> Result<(), String> {
    let dir = install_dir()?;
    std::process::Command::new("explorer.exe")
        .arg(&dir)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("cannot open {}: {error}", dir.display()))
}

#[cfg(not(windows))]
pub fn reveal() -> Result<(), String> {
    Err("opening the bridge folder is currently Windows-only".to_string())
}

fn sync(source: &Path, target: &Path, version: &str) -> Result<(), String> {
    if is_current(target, version) {
        return Ok(());
    }

    // The NSIS bundle installs into `%LOCALAPPDATA%\ai-quota-deck`, which is the
    // staging directory itself, so the installer has already put the files in
    // place and each copy would be a file onto itself — Windows answers that
    // with a sharing violation. Only the stamp is still ours to write.
    if is_same_dir(source, target) {
        return fs::write(target.join(STAMP), version)
            .map_err(|error| format!("cannot stamp {}: {error}", target.display()));
    }

    stage_and_swap(source, target, version)
}

fn stage_and_swap(source: &Path, target: &Path, version: &str) -> Result<(), String> {
    let parent = target
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", target.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    let staging = target.with_extension(format!("stage-{}", std::process::id()));
    let backup = target.with_extension(format!("old-{}", std::process::id()));
    let _ = fs::remove_dir_all(&staging);
    let _ = fs::remove_dir_all(&backup);

    let staged = (|| {
        for name in FILES {
            let from = source.join(name);
            let to = staging.join(name);
            if let Some(parent) = to.parent() {
                fs::create_dir_all(parent)
                    .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
            }
            fs::copy(&from, &to).map_err(|error| {
                format!(
                    "cannot stage {} as {}: {error}",
                    from.display(),
                    to.display()
                )
            })?;
        }
        fs::write(staging.join(STAMP), version)
            .map_err(|error| format!("cannot stamp {}: {error}", staging.display()))
    })();
    if let Err(error) = staged {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }

    if target.exists() {
        if let Err(error) = fs::rename(target, &backup) {
            let _ = fs::remove_dir_all(&staging);
            return Err(format!("cannot set aside {}: {error}", target.display()));
        }
    }
    if let Err(error) = fs::rename(&staging, target) {
        let restored = if backup.exists() {
            fs::rename(&backup, target)
        } else {
            Ok(())
        };
        let _ = fs::remove_dir_all(&staging);
        return match restored {
            Ok(()) => Err(format!("cannot activate {}: {error}", target.display())),
            Err(restore_error) => Err(format!(
                "cannot activate {} ({error}) or restore it ({restore_error})",
                target.display()
            )),
        };
    }
    let _ = fs::remove_dir_all(&backup);
    Ok(())
}

/// Compared after canonicalising, so an install root reached by a different but
/// equivalent path still counts as the same directory. An unreadable path means
/// the target does not exist yet, which is a normal first run, not a match.
fn is_same_dir(source: &Path, target: &Path) -> bool {
    match (source.canonicalize(), target.canonicalize()) {
        (Ok(source), Ok(target)) => source == target,
        _ => false,
    }
}

fn is_current(target: &Path, version: &str) -> bool {
    if !target.join("manifest.json").is_file() {
        return false;
    }
    fs::read_to_string(target.join(STAMP)).is_ok_and(|stamped| stamped == version)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(source: &Path) {
        for name in FILES {
            let path = source.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, format!("// {name}")).unwrap();
        }
        fs::write(source.join("package.json"), "{}").unwrap();
        fs::create_dir_all(source.join("tests")).unwrap();
        fs::write(source.join("tests/parsers.test.js"), "// test").unwrap();
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ai-quota-deck-bridge-{name}"));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn install_dir_is_the_documented_fixed_path() {
        let dir = install_dir().unwrap();
        assert!(
            dir.ends_with("ai-quota-deck/browser-bridge")
                || dir.ends_with(r"ai-quota-deck\browser-bridge")
        );
    }

    #[test]
    fn copies_only_the_extension_files() {
        let root = temp("copies");
        let (source, target) = (root.join("src"), root.join("dst"));
        seed(&source);

        sync(&source, &target, "0.1.0").unwrap();

        for name in FILES {
            assert!(target.join(name).is_file(), "{name} was not copied");
        }
        assert!(!target.join("package.json").exists());
        assert!(!target.join("tests").exists());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn recopies_when_the_app_version_changes() {
        let root = temp("version");
        let (source, target) = (root.join("src"), root.join("dst"));
        seed(&source);
        sync(&source, &target, "0.1.0").unwrap();

        fs::write(source.join("manifest.json"), "// rebuilt").unwrap();
        sync(&source, &target, "0.1.0").unwrap();
        assert_eq!(
            fs::read_to_string(target.join("manifest.json")).unwrap(),
            "// manifest.json",
            "same version should not recopy"
        );

        sync(&source, &target, "0.2.0").unwrap();
        assert_eq!(
            fs::read_to_string(target.join("manifest.json")).unwrap(),
            "// rebuilt",
            "new version should recopy"
        );
        fs::remove_dir_all(&root).unwrap();
    }

    /// The NSIS bundle installs to `%LOCALAPPDATA%\ai-quota-deck`, which is also
    /// where the bridge is staged — so source and target are the same directory
    /// and every copy is a file onto itself.
    #[test]
    fn staging_onto_itself_is_not_an_error() {
        let root = temp("selfcopy");
        seed(&root);

        sync(&root, &root, "0.1.0").unwrap();

        for name in FILES {
            assert!(root.join(name).is_file(), "{name} disappeared");
        }
        assert_eq!(
            fs::read_to_string(root.join("manifest.json")).unwrap(),
            "// manifest.json",
            "the file must not be truncated by copying onto itself"
        );
        assert_eq!(
            fs::read_to_string(root.join(STAMP)).unwrap(),
            "0.1.0",
            "the stamp must still be written, or every launch retries and fails"
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn recopies_when_the_user_deleted_the_folder() {
        let root = temp("deleted");
        let (source, target) = (root.join("src"), root.join("dst"));
        seed(&source);
        sync(&source, &target, "0.1.0").unwrap();

        fs::remove_file(target.join("manifest.json")).unwrap();
        sync(&source, &target, "0.1.0").unwrap();

        assert!(target.join("manifest.json").is_file());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_failed_stage_leaves_the_installed_bridge_whole() {
        let root = temp("failed-stage");
        let (source, target) = (root.join("src"), root.join("dst"));
        seed(&source);
        sync(&source, &target, "0.1.0").unwrap();
        fs::write(source.join("manifest.json"), "// new manifest").unwrap();
        fs::remove_file(source.join("src/grok.js")).unwrap();

        assert!(sync(&source, &target, "0.2.0").is_err());
        assert_eq!(
            fs::read_to_string(target.join("manifest.json")).unwrap(),
            "// manifest.json"
        );
        assert_eq!(fs::read_to_string(target.join(STAMP)).unwrap(), "0.1.0");
        assert!(target.join("src/grok.js").is_file());
        fs::remove_dir_all(&root).unwrap();
    }
}
