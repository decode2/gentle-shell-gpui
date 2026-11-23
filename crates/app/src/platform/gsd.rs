//! `gsd`: the user's command for opening Gentle Shell Desktop from a
//! terminal, like `gs` opens Gentle Shell. `gsd` opens the current folder as
//! a workspace and `gsd <folder>` that folder; with the app already running,
//! the single-instance server switches its window to that workspace.
//!
//! The command is a generated shim that runs this executable with `--cwd`
//! and returns to the prompt at once. Unlike the agents' shims it goes on
//! the user's own PATH, but only when they ask (palette → Install `gsd`
//! command) or through the Windows installer, which ships its own copy.

use std::path::{Path, PathBuf};

/// The line that marks a `gsd` file as ours, so it is never overwritten or
/// refreshed when it belongs to something else.
const MARKER: &str = "gentle-shell-gpui gsd shim";

/// Where the app keeps the shim: `~/.local/bin` on macOS and Linux (usually on
/// PATH already), its own data folder on Windows.
fn shim_path() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        Some(
            dirs::data_dir()?
                .join("gentle-shell-gpui")
                .join("bin")
                .join("gsd.cmd"),
        )
    }
    #[cfg(not(windows))]
    {
        Some(dirs::home_dir()?.join(".local").join("bin").join("gsd"))
    }
}

#[cfg(windows)]
fn shim_body(exe: &Path) -> String {
    // `start` returns to the prompt instead of waiting for the window; cmd.exe
    // would expand a `%` in the path.
    let exe = exe.to_string_lossy().replace('%', "%%");
    format!("@echo off\r\nrem {MARKER}\r\nstart \"\" \"{exe}\" --cwd %*\r\n")
}

#[cfg(not(windows))]
fn shim_body(exe: &Path) -> String {
    let exe = exe.to_string_lossy().replace('\'', "'\\''");
    format!("#!/bin/sh\n# {MARKER}\n'{exe}' --cwd \"$@\" >/dev/null 2>&1 &\n")
}

fn is_ours(path: &Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|body| body.contains(MARKER))
}

fn write_shim(path: &Path, exe: &Path) -> std::io::Result<()> {
    let body = shim_body(exe);
    if std::fs::read_to_string(path).ok().as_deref() == Some(body.as_str()) {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, body)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

/// At startup: point an installed shim at this executable again (an update
/// or a moved app changes its path). Never creates one.
pub fn refresh() {
    let (Some(path), Ok(exe)) = (shim_path(), std::env::current_exe()) else {
        return;
    };
    if is_ours(&path) {
        let _ = write_shim(&path, &exe);
    }
}

/// Palette → Install `gsd` command. Blocking (Windows edits the user PATH
/// through PowerShell): call off the UI thread. The text says what happened.
pub fn install() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    // The Windows installer ships `bin\gsd.cmd` next to the exe: only make
    // sure its folder is on PATH.
    #[cfg(windows)]
    if let Some(bin) = exe.parent().map(|dir| dir.join("bin"))
        && bin.join("gsd.cmd").is_file()
    {
        return add_to_path(&bin);
    }
    let path = shim_path().ok_or("This system has no home or app data folder.")?;
    if path.exists() && !is_ours(&path) {
        return Err(format!(
            "{} already exists and isn't Gentle Shell Desktop's; remove it first.",
            path.display()
        ));
    }
    write_shim(&path, &exe).map_err(|e| format!("Could not write {}: {e}", path.display()))?;
    let dir = path.parent().unwrap_or(&path);
    add_to_path(dir)
}

#[cfg(windows)]
fn add_to_path(dir: &Path) -> Result<String, String> {
    // Read and write the raw value: `[Environment]::SetEnvironmentVariable`
    // would expand `%USERPROFILE%`-style entries for good. Setting and
    // clearing a dummy variable through it broadcasts the change, so new
    // terminals see it without signing out.
    const SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$dir = $env:GSD_DIR
$key = Get-Item 'HKCU:\Environment'
$raw = $key.GetValue('Path', '', 'DoNotExpandEnvironmentNames')
$entries = @($raw -split ';' | Where-Object { $_ -ne '' })
if ($entries | Where-Object { $_.TrimEnd('\') -ieq $dir.TrimEnd('\') }) { 'present'; exit 0 }
$value = (@($entries) + $dir) -join ';'
[Microsoft.Win32.Registry]::SetValue('HKEY_CURRENT_USER\Environment', 'Path', $value, 'ExpandString')
[Environment]::SetEnvironmentVariable('GSD_PATH_BROADCAST', '1', 'User')
[Environment]::SetEnvironmentVariable('GSD_PATH_BROADCAST', $null, 'User')
'added'
"#;
    let out = pi_rpc::command("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", SCRIPT])
        .env("GSD_DIR", dir)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("Could not run PowerShell: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "Could not add {} to your PATH: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(
        if String::from_utf8_lossy(&out.stdout).trim() == "present" {
            "gsd is installed. Type gsd in any folder to open it here.".into()
        } else {
            "gsd is installed. Open a new terminal and type gsd in any folder.".into()
        },
    )
}

#[cfg(not(windows))]
fn add_to_path(dir: &Path) -> Result<String, String> {
    let on_path =
        std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|p| p == dir));
    Ok(if on_path {
        "gsd is installed. Type gsd in any folder to open it here.".into()
    } else {
        format!(
            "gsd is installed in {}, which isn't on your PATH yet: add it in your shell profile.",
            dir.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::{is_ours, shim_body, write_shim};

    #[test]
    fn shim_passes_cwd_and_is_recognized_as_ours() {
        let dir = std::env::temp_dir().join(format!("gsd-shim-test-{}", std::process::id()));
        let path = dir.join("gsd");
        let exe = std::path::Path::new("/Apps/Gentle Shell/gentle-shell-gpui");
        write_shim(&path, exe).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert_eq!(body, shim_body(exe));
        assert!(body.contains("--cwd") && body.contains("Gentle Shell"));
        assert!(is_ours(&path));

        // Someone else's `gsd` is never taken for ours.
        std::fs::write(&path, "#!/bin/sh\necho other\n").unwrap();
        assert!(!is_ours(&path));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_shim_escapes_percent_and_returns_to_the_prompt() {
        let body = shim_body(std::path::Path::new(r"C:@%\Gentle Shell\gs.exe"));
        assert!(body.contains(r#"start "" "C:@%%\Gentle Shell\gs.exe" --cwd %*"#));
    }

    #[cfg(not(windows))]
    #[test]
    fn unix_shim_quotes_the_path_and_runs_in_the_background() {
        let body = shim_body(std::path::Path::new("/Apps/It's/gs"));
        assert!(body.contains(r#"'/Apps/It'''s/gs' --cwd "$@" >/dev/null 2>&1 &"#));
    }

    #[test]
    fn rewriting_the_same_shim_leaves_it_untouched() {
        let dir = std::env::temp_dir().join(format!("gsd-idem-test-{}", std::process::id()));
        let path = dir.join("gsd");
        let exe = std::path::Path::new("/x/gs");
        write_shim(&path, exe).unwrap();
        let first = std::fs::metadata(&path).unwrap().modified().unwrap();
        write_shim(&path, exe).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), first);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
