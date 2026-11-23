//! Command line: `gentle-shell-gpui [FOLDER] [--cwd] [-p|--prompt TEXT] [--open-url URL]`.

use std::path::PathBuf;

/// `gentle-shell-gpui [FOLDER] [--cwd] [-p|--prompt TEXT]`
pub struct Cli {
    pub folder: Option<PathBuf>,
    /// `--cwd`: without `FOLDER`, open the current directory (what `gsd`
    /// passes). A plain launch ignores it: the Start menu or Finder start in
    /// some unrelated folder, and the last workspace is restored instead.
    pub here: bool,
    pub prompt: Option<String>,
    /// `--open-url URL`: show a page in the running app's browser.
    pub url: Option<String>,
}

pub fn parse_cli() -> Cli {
    let mut cli = Cli {
        folder: None,
        here: false,
        prompt: None,
        url: None,
    };
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        if arg == "-p" || arg == "--prompt" {
            cli.prompt = args.next().map(|p| p.to_string_lossy().into_owned());
        } else if arg == "--cwd" {
            cli.here = true;
        } else if arg == "--open-url" || arg == "--url" {
            cli.url = Some(
                args.next()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            );
        } else if cli.folder.is_none() {
            cli.folder = Some(PathBuf::from(arg));
        }
    }
    cli
}

/// The workspace folder the command line asks for: `folder`, else with
/// `here` the current directory `cwd`; only an existing directory, canonical.
pub fn resolve_folder(
    folder: Option<PathBuf>,
    here: bool,
    cwd: Option<PathBuf>,
) -> Option<PathBuf> {
    folder
        .or(if here { cwd } else { None })
        .filter(|p| p.is_dir())
        .and_then(|p| dunce::canonicalize(p).ok())
}

/// The folder this launch asks for (see [`resolve_folder`]).
pub fn requested_folder(cli: &Cli) -> Option<PathBuf> {
    resolve_folder(cli.folder.clone(), cli.here, std::env::current_dir().ok())
}

/// The folder the first tab opens: the one the command line asks for, else
/// the current directory, else home (GUI launches start in `/`).
pub fn default_cwd() -> PathBuf {
    requested_folder(&parse_cli())
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .filter(|p| p.parent().is_some())
        })
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::resolve_folder;

    #[test]
    fn here_flag_uses_the_current_directory_only_without_a_folder() {
        let base = std::env::temp_dir().join(format!("gsd-cli-test-{}", std::process::id()));
        let (here, there) = (base.join("here"), base.join("there"));
        std::fs::create_dir_all(&here).unwrap();
        std::fs::create_dir_all(&there).unwrap();
        let canon = |p: &std::path::Path| dunce::canonicalize(p).unwrap();

        // `gsd` in a folder: that folder.
        assert_eq!(
            resolve_folder(None, true, Some(here.clone())),
            Some(canon(&here))
        );
        // `gsd <folder>`: the folder wins over the current directory.
        assert_eq!(
            resolve_folder(Some(there.clone()), true, Some(here.clone())),
            Some(canon(&there))
        );
        // A plain launch (Start menu, Finder) keeps restoring the last workspace.
        assert_eq!(resolve_folder(None, false, Some(here.clone())), None);
        // A folder that doesn't exist is ignored, as before.
        assert_eq!(
            resolve_folder(Some(base.join("missing")), false, None),
            None
        );
        std::fs::remove_dir_all(&base).unwrap();
    }
}
