use std::env;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

pub(crate) fn resolve_codex_executable(explicit: Option<&Path>) -> Option<PathBuf> {
    discover_executable(
        explicit,
        env::var_os("OPSAIL_CODEX_PATH").as_deref(),
        env::var_os("PATH").as_deref(),
    )
}

fn discover_executable(
    explicit: Option<&Path>,
    override_path: Option<&OsStr>,
    search_path: Option<&OsStr>,
) -> Option<PathBuf> {
    if let Some(path) = explicit {
        return is_executable(path).then(|| path.to_path_buf());
    }

    if let Some(path) = override_path.filter(|path| !path.is_empty()) {
        let path = PathBuf::from(path);
        return is_executable(&path).then_some(path);
    }

    search_path.and_then(|path| {
        env::split_paths(path)
            .flat_map(|directory| {
                executable_names()
                    .iter()
                    .map(move |name| directory.join(name))
            })
            .find(|candidate| is_executable(candidate))
    })
}

fn executable_names() -> &'static [&'static str] {
    executable_names_for(cfg!(windows))
}

fn executable_names_for(windows: bool) -> &'static [&'static str] {
    if windows {
        &["codex.exe", "codex.cmd", "codex.bat"]
    } else {
        &["codex"]
    }
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    use tempfile::tempdir;

    use super::{discover_executable, executable_names_for};

    fn write_executable(path: &std::path::Path) {
        fs::write(path, b"#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            let mut permissions = fs::metadata(path).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(path, permissions).unwrap();
        }
    }

    #[test]
    fn explicit_path_wins_when_executable() {
        let directory = tempdir().unwrap();
        let path = directory
            .path()
            .join(if cfg!(windows) { "codex.exe" } else { "codex" });
        write_executable(&path);
        assert_eq!(
            discover_executable(Some(&path), Some("ignored".as_ref()), None),
            Some(path)
        );
    }

    #[test]
    fn missing_explicit_path_does_not_fall_through() {
        let path = PathBuf::from("/opsail-missing-codex/codex");
        assert_eq!(
            discover_executable(Some(&path), Some("/tmp/also-missing".as_ref()), None),
            None
        );
    }

    #[test]
    fn override_path_is_used_before_search_path() {
        let directory = tempdir().unwrap();
        let override_path = directory.path().join("override-codex");
        let search_dir = directory.path().join("bin");
        fs::create_dir(&search_dir).unwrap();
        write_executable(&override_path);
        write_executable(&search_dir.join("codex"));
        assert_eq!(
            discover_executable(
                None,
                Some(override_path.as_os_str()),
                Some(search_dir.as_os_str())
            ),
            Some(override_path)
        );
    }

    #[test]
    fn search_path_finds_codex() {
        let directory = tempdir().unwrap();
        let bin = directory.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let path = bin.join(if cfg!(windows) { "codex.exe" } else { "codex" });
        write_executable(&path);
        assert_eq!(
            discover_executable(None, None, Some(bin.as_os_str())),
            Some(path)
        );
    }

    #[test]
    fn windows_search_includes_npm_and_pnpm_command_shims() {
        assert_eq!(
            executable_names_for(true),
            &["codex.exe", "codex.cmd", "codex.bat"]
        );
    }
}
