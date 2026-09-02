use std::{
    collections::HashSet,
    ffi::OsString,
    fs,
    os::unix::ffi::OsStrExt,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
};

use thiserror::Error;

const OVERRIDE_ENV: &str = "MUXFLOW_TMUX_PATH";
#[cfg(target_os = "macos")]
const MAX_PATH_FILE_BYTES: u64 = 64 * 1024;

static AUTOMATIC_TMUX: Mutex<Option<PathBuf>> = Mutex::new(None);

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum TmuxExecutableError {
    #[error("MUXFLOW_TMUX_PATH must name an absolute executable file")]
    InvalidOverride,
    #[error(
        "tmux executable was not found; install tmux or set MUXFLOW_TMUX_PATH to an absolute executable path"
    )]
    NotFound,
}

/// Why a program could not be resolved, naming the program and its override
/// variable so the same message shape serves tmux and any other helper the
/// host has to find without a shell.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum ExecutableError {
    #[error("{override_env} must name an absolute executable file")]
    InvalidOverride { override_env: &'static str },
    #[error(
        "{program} executable was not found; install {program} or set {override_env} to an absolute executable path"
    )]
    NotFound {
        program: &'static str,
        override_env: &'static str,
    },
}

impl From<ExecutableError> for TmuxExecutableError {
    fn from(error: ExecutableError) -> Self {
        match error {
            ExecutableError::InvalidOverride { .. } => Self::InvalidOverride,
            ExecutableError::NotFound { .. } => Self::NotFound,
        }
    }
}

/// Resolves the tmux client without starting a shell.
///
/// GUI applications on macOS inherit a system-only `PATH`, so Homebrew and
/// other package-manager prefixes are searched explicitly after the process
/// path and the system's path registry. A valid explicit override always wins.
pub fn tmux_executable() -> Result<PathBuf, TmuxExecutableError> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    resolve_executable(
        OVERRIDE_ENV,
        "tmux",
        || known_installer_candidates(home.as_deref()),
        &AUTOMATIC_TMUX,
    )
    .map_err(Into::into)
}

pub fn tmux_command() -> Result<Command, TmuxExecutableError> {
    Ok(Command::new(tmux_executable()?))
}

/// Finds `program` the way [`tmux_executable`] finds tmux: a valid
/// `override_env` wins outright and an invalid one never falls back; otherwise
/// every absolute `PATH` entry, the platform's registered path files, then the
/// caller's `installer` candidates, in that order, and the first executable
/// found is remembered in `cache` until it stops being executable. The
/// candidates are built only on a cache miss: this sits on the tmux hot path.
pub fn resolve_executable(
    override_env: &'static str,
    program: &'static str,
    installer: impl FnOnce() -> Vec<PathBuf>,
    cache: &Mutex<Option<PathBuf>>,
) -> Result<PathBuf, ExecutableError> {
    if let Some(path) = resolve_override(std::env::var_os(override_env), override_env)? {
        return Ok(path);
    }
    resolve_automatic(cache, || automatic_candidates(program, installer())).ok_or(
        ExecutableError::NotFound {
            program,
            override_env,
        },
    )
}

fn resolve_automatic(
    cache: &Mutex<Option<PathBuf>>,
    candidates: impl FnOnce() -> Vec<PathBuf>,
) -> Option<PathBuf> {
    let Ok(mut cached) = cache.lock() else {
        return first_executable(candidates());
    };
    if let Some(path) = cached.as_ref().filter(|path| executable(path)) {
        return Some(path.clone());
    }

    let path = first_executable(candidates())?;
    *cached = Some(path.clone());
    Some(path)
}

fn automatic_candidates(program: &str, installer: Vec<PathBuf>) -> Vec<PathBuf> {
    let path = std::env::var_os("PATH");
    #[cfg(target_os = "macos")]
    let registered = macos_registered_candidates(program);
    #[cfg(not(target_os = "macos"))]
    let registered = Vec::new();

    automatic_candidates_from(program, path.as_deref(), registered, installer)
}

fn automatic_candidates_from(
    program: &str,
    path: Option<&std::ffi::OsStr>,
    registered: impl IntoIterator<Item = PathBuf>,
    installer: impl IntoIterator<Item = PathBuf>,
) -> Vec<PathBuf> {
    let mut candidates = path
        .map(|path| path_candidates(path, program))
        .unwrap_or_default();
    candidates.extend(registered);
    candidates.extend(installer);
    candidates
}

fn known_installer_candidates(home: Option<&Path>) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    #[cfg(target_os = "macos")]
    candidates.extend([
        PathBuf::from("/opt/homebrew/bin/tmux"),
        PathBuf::from("/usr/local/bin/tmux"),
        PathBuf::from("/opt/local/bin/tmux"),
        PathBuf::from("/opt/pkg/bin/tmux"),
        PathBuf::from("/run/current-system/sw/bin/tmux"),
        PathBuf::from("/nix/var/nix/profiles/default/bin/tmux"),
    ]);

    #[cfg(target_os = "linux")]
    candidates.extend([
        PathBuf::from("/usr/local/bin/tmux"),
        PathBuf::from("/usr/bin/tmux"),
        PathBuf::from("/bin/tmux"),
        PathBuf::from("/home/linuxbrew/.linuxbrew/bin/tmux"),
        PathBuf::from("/run/current-system/sw/bin/tmux"),
        PathBuf::from("/nix/var/nix/profiles/default/bin/tmux"),
        PathBuf::from("/usr/pkg/bin/tmux"),
        PathBuf::from("/snap/bin/tmux"),
    ]);

    if let Some(home) = home {
        candidates.extend([
            home.join(".local/bin/tmux"),
            home.join(".nix-profile/bin/tmux"),
            home.join(".linuxbrew/bin/tmux"),
        ]);
    }
    candidates
}

fn resolve_override(
    value: Option<OsString>,
    override_env: &'static str,
) -> Result<Option<PathBuf>, ExecutableError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let path = PathBuf::from(value);
    executable(&path)
        .then_some(Some(path))
        .ok_or(ExecutableError::InvalidOverride { override_env })
}

fn path_candidates(path: &std::ffi::OsStr, program: &str) -> Vec<PathBuf> {
    std::env::split_paths(path)
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join(program))
        .collect()
}

#[cfg(target_os = "macos")]
fn macos_registered_candidates(program: &str) -> Vec<PathBuf> {
    let mut files = vec![PathBuf::from("/etc/paths")];
    if let Ok(entries) = fs::read_dir("/etc/paths.d") {
        let mut registered = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        registered.sort();
        files.extend(registered);
    }
    files
        .iter()
        .flat_map(|file| path_file_candidates(file, program))
        .collect()
}

#[cfg(target_os = "macos")]
fn path_file_candidates(file: &Path, program: &str) -> Vec<PathBuf> {
    if !fs::metadata(file)
        .is_ok_and(|metadata| metadata.is_file() && metadata.len() <= MAX_PATH_FILE_BYTES)
    {
        return Vec::new();
    }
    let Ok(contents) = fs::read_to_string(file) else {
        return Vec::new();
    };
    contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join(program))
        .collect()
}

fn first_executable(candidates: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    let mut seen = HashSet::<OsString>::new();
    candidates
        .into_iter()
        .find(|candidate| seen.insert(candidate.as_os_str().to_owned()) && executable(candidate))
}

fn executable(path: &Path) -> bool {
    if !path.is_absolute()
        || !fs::metadata(path)
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    {
        return false;
    }
    let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `path` is a live NUL-terminated string. Muxflow is never setuid,
    // so access(2)'s real-user check is the permission the child process gets.
    unsafe { libc::access(path.as_ptr(), libc::X_OK) == 0 }
}

#[cfg(test)]
mod tests {
    use std::{
        io::Write as _,
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::*;

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

    struct TestRoot(PathBuf);

    impl TestRoot {
        fn new() -> Self {
            let serial = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "muxflow-tmux-resolver-{}-{serial}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn executable(&self, relative: &str) -> PathBuf {
            let path = self.0.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::File::create(&path)
                .unwrap()
                .write_all(b"#!/bin/sh\nexit 0\n")
                .unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            path
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn first_executable_preserves_precedence_and_skips_duplicates() {
        let root = TestRoot::new();
        let first = root.executable("first/tmux");
        let second = root.executable("second/tmux");
        assert_eq!(
            first_executable([first.clone(), first.clone(), second]),
            Some(first)
        );
    }

    #[test]
    fn stale_cached_executable_is_replaced() {
        let root = TestRoot::new();
        let first = root.executable("first/tmux");
        let second = root.executable("second/tmux");
        let cache = Mutex::new(Some(first.clone()));

        fs::remove_file(first).unwrap();

        assert_eq!(
            resolve_automatic(&cache, || vec![second.clone()]),
            Some(second.clone())
        );
        assert_eq!(*cache.lock().unwrap(), Some(second));
    }

    #[test]
    fn executable_requires_an_absolute_regular_file_with_an_execute_bit() {
        let root = TestRoot::new();
        let plain = root.0.join("plain");
        fs::write(&plain, b"not executable").unwrap();
        let directory = root.0.join("directory");
        fs::create_dir(&directory).unwrap();
        assert!(!executable(Path::new("relative/tmux")));
        assert!(!executable(&plain));
        assert!(!executable(&directory));
        assert!(executable(&root.executable("bin/tmux")));
    }

    #[test]
    fn explicit_override_wins_and_never_falls_back_when_invalid() {
        let root = TestRoot::new();
        let overridden = root.executable("custom/tmux");
        assert_eq!(
            resolve_override(Some(overridden.clone().into_os_string()), OVERRIDE_ENV),
            Ok(Some(overridden))
        );
        assert_eq!(
            resolve_override(Some(OsString::from("relative/tmux")), OVERRIDE_ENV),
            Err(ExecutableError::InvalidOverride {
                override_env: OVERRIDE_ENV
            })
        );
        assert_eq!(resolve_override(None, OVERRIDE_ENV), Ok(None));
        assert_eq!(
            TmuxExecutableError::from(ExecutableError::NotFound {
                program: "tmux",
                override_env: OVERRIDE_ENV
            }),
            TmuxExecutableError::NotFound
        );
    }

    /// The generalized resolver behind `tmux_executable()`: an override wins
    /// over every candidate, an invalid one never falls back, and a missing
    /// program names itself and its variable.
    #[test]
    fn resolve_executable_honours_override_precedence_and_names_the_program() {
        let root = TestRoot::new();
        // A name nothing on the developer's PATH answers to, so the installer
        // candidate is what gets found.
        let installed = root.executable("installer/muxflow-resolver-probe");
        let cache = Mutex::new(None);
        // No override: the installer candidate is found and cached.
        assert_eq!(
            resolve_executable(
                "MUXFLOW_TEST_UV_UNSET",
                "muxflow-resolver-probe",
                || vec![installed.clone()],
                &cache
            ),
            Ok(installed.clone())
        );
        assert_eq!(*cache.lock().unwrap(), Some(installed));
        let missing = resolve_executable(
            "MUXFLOW_TEST_UV_UNSET",
            "definitely-not-installed-xyz",
            Vec::new,
            &Mutex::new(None),
        )
        .unwrap_err();
        assert_eq!(
            missing,
            ExecutableError::NotFound {
                program: "definitely-not-installed-xyz",
                override_env: "MUXFLOW_TEST_UV_UNSET"
            }
        );
        assert!(missing.to_string().contains("definitely-not-installed-xyz"));
        assert!(missing.to_string().contains("MUXFLOW_TEST_UV_UNSET"));
    }

    #[test]
    fn path_candidates_keep_absolute_path_order_and_ignore_relative_entries() {
        let root = TestRoot::new();
        let first = root.0.join("first");
        let second = root.0.join("second");
        let encoded = std::env::join_paths([&first, Path::new("relative"), &second]).unwrap();
        assert_eq!(
            path_candidates(&encoded, "tmux"),
            [first.join("tmux"), second.join("tmux")]
        );
    }

    #[test]
    fn installer_candidates_cover_system_and_user_profiles() {
        let home = Path::new("/home/example");
        let candidates = known_installer_candidates(Some(home));
        assert!(candidates.contains(&home.join(".local/bin/tmux")));
        assert!(candidates.contains(&home.join(".nix-profile/bin/tmux")));
        assert!(candidates.contains(&PathBuf::from("/run/current-system/sw/bin/tmux")));
        assert!(candidates.contains(&PathBuf::from("/nix/var/nix/profiles/default/bin/tmux")));
        #[cfg(target_os = "macos")]
        for path in [
            "/opt/homebrew/bin/tmux",
            "/usr/local/bin/tmux",
            "/opt/local/bin/tmux",
            "/opt/pkg/bin/tmux",
        ] {
            assert!(candidates.contains(&PathBuf::from(path)), "{path}");
        }
        #[cfg(target_os = "linux")]
        assert!(candidates.contains(&PathBuf::from("/usr/pkg/bin/tmux")));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn dock_path_falls_through_to_an_installer_candidate() {
        let root = TestRoot::new();
        let fallback = root.executable("installer/tmux");
        let dock_path = OsString::from("/usr/bin:/bin:/usr/sbin:/sbin");
        assert_eq!(
            first_executable(automatic_candidates_from(
                "tmux",
                Some(&dock_path),
                Vec::new(),
                [fallback.clone()],
            )),
            Some(fallback)
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn registered_path_files_are_bounded_and_accept_only_absolute_directories() {
        let root = TestRoot::new();
        let file = root.0.join("paths");
        fs::write(&file, "/one/bin\nrelative\n\n/two/bin\n").unwrap();
        assert_eq!(
            path_file_candidates(&file, "tmux"),
            [
                PathBuf::from("/one/bin/tmux"),
                PathBuf::from("/two/bin/tmux")
            ]
        );

        fs::write(&file, vec![b'x'; MAX_PATH_FILE_BYTES as usize + 1]).unwrap();
        assert!(path_file_candidates(&file, "tmux").is_empty());
    }
}
