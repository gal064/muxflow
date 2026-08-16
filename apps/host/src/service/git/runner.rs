use std::{
    cell::RefCell,
    ffi::{OsStr, OsString},
    fs::File,
    io::{Read, Write},
    ops::Deref,
    os::unix::{
        ffi::OsStringExt,
        fs::MetadataExt as _,
        io::{AsRawFd as _, FromRawFd as _},
        process::CommandExt,
    },
    process::{Child, Command, Output, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

#[cfg(any(target_os = "macos", target_os = "ios"))]
use std::{ffi::CString, os::unix::ffi::OsStrExt as _, path::PathBuf};

use anyhow::{Context, bail};
#[cfg(test)]
use tmux_agent_protocol::v1;

use super::{MAX_GIT_OUTPUT, validate_git_path};

const GIT_DEADLINE: Duration = Duration::from_secs(30);
pub(super) const GIT_MUTATION_DEADLINE: Duration = Duration::from_secs(2 * 60);
pub(super) const GIT_COMMIT_DEADLINE: Duration = Duration::from_secs(5 * 60);
const TERMINATION_GRACE: Duration = Duration::from_millis(250);

thread_local! {
    static GIT_METADATA_ENV: RefCell<Vec<GitMetadataBinding>> = const { RefCell::new(Vec::new()) };
}

#[derive(Clone)]
struct GitMetadataBinding {
    #[cfg(target_os = "linux")]
    git_dir: OsString,
    #[cfg(target_os = "linux")]
    common_dir: OsString,
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    git_fd: i32,
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    common_fd: i32,
}

pub(super) struct GitMetadataCapability {
    git_dir: File,
    common_dir: File,
}

pub(super) struct GitMetadataGuard<'a> {
    _capability: &'a GitMetadataCapability,
}

impl GitMetadataCapability {
    pub(super) fn capture(git_dir: &[u8], common_dir: &[u8]) -> anyhow::Result<Self> {
        Ok(Self {
            git_dir: open_metadata_directory(git_dir)?,
            common_dir: open_metadata_directory(common_dir)?,
        })
    }

    pub(super) fn try_clone(&self) -> anyhow::Result<Self> {
        let git_dir = self.git_dir.try_clone()?;
        let common_dir = self.common_dir.try_clone()?;
        retain_exec_fd(git_dir.as_raw_fd())?;
        retain_exec_fd(common_dir.as_raw_fd())?;
        Ok(Self {
            git_dir,
            common_dir,
        })
    }

    pub(super) fn identities(&self) -> anyhow::Result<((u64, u64), (u64, u64))> {
        let git = self.git_dir.metadata()?;
        let common = self.common_dir.metadata()?;
        Ok(((git.dev(), git.ino()), (common.dev(), common.ino())))
    }

    pub(super) fn install(&self) -> GitMetadataGuard<'_> {
        GIT_METADATA_ENV.with(|environment| {
            environment.borrow_mut().push(GitMetadataBinding {
                #[cfg(target_os = "linux")]
                git_dir: descriptor_directory(self.git_dir.as_raw_fd()),
                #[cfg(target_os = "linux")]
                common_dir: descriptor_directory(self.common_dir.as_raw_fd()),
                #[cfg(any(target_os = "macos", target_os = "ios"))]
                git_fd: self.git_dir.as_raw_fd(),
                #[cfg(any(target_os = "macos", target_os = "ios"))]
                common_fd: self.common_dir.as_raw_fd(),
            });
        });
        GitMetadataGuard { _capability: self }
    }

    pub(super) fn stable_paths(&self) -> (OsString, OsString) {
        (
            descriptor_directory(self.git_dir.as_raw_fd()),
            descriptor_directory(self.common_dir.as_raw_fd()),
        )
    }
}

impl Drop for GitMetadataGuard<'_> {
    fn drop(&mut self) {
        GIT_METADATA_ENV.with(|environment| {
            environment.borrow_mut().pop();
        });
    }
}

fn open_metadata_directory(path: &[u8]) -> anyhow::Result<File> {
    let path = std::ffi::CString::new(path).context("Git metadata path contains NUL")?;
    // SAFETY: path is live and successful open returns a uniquely owned fd.
    let fd = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).context("Git metadata directory unavailable");
    }
    retain_exec_fd(fd)?;
    // SAFETY: the successful open returned a uniquely owned fd.
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn retain_exec_fd(fd: i32) -> anyhow::Result<()> {
    // SAFETY: fd is live; clearing CLOEXEC keeps the capability usable by Git.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, 0) } < 0 {
        bail!(
            "failed to retain Git metadata capability: {}",
            std::io::Error::last_os_error()
        );
    }
    Ok(())
}

fn descriptor_directory(fd: i32) -> OsString {
    #[cfg(target_os = "linux")]
    return OsString::from(format!("/proc/self/fd/{fd}"));
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        let mut buffer = [0_u8; libc::PATH_MAX as usize];
        // SAFETY: F_GETPATH writes the live metadata directory's vnode path
        // into this fixed-size buffer. Identity is revalidated around use.
        if unsafe { libc::fcntl(fd, libc::F_GETPATH, buffer.as_mut_ptr()) } == 0 {
            let length = buffer
                .iter()
                .position(|byte| *byte == 0)
                .unwrap_or(buffer.len());
            return OsString::from_vec(buffer[..length].to_vec());
        }
        OsString::from(format!("/dev/fd/{fd}"))
    }
}

pub(super) struct GitOutput {
    pub(super) output: Output,
    pub(super) stdout_truncated: bool,
    pub(super) stderr_truncated: bool,
    pub(super) interrupted: Option<String>,
}

impl Deref for GitOutput {
    type Target = Output;

    fn deref(&self) -> &Self::Target {
        &self.output
    }
}

impl From<Output> for GitOutput {
    fn from(output: Output) -> Self {
        Self {
            output,
            stdout_truncated: false,
            stderr_truncated: false,
            interrupted: None,
        }
    }
}

pub(super) fn git_path_cancellable(
    root: &str,
    prefix: &[&[u8]],
    path: &[u8],
    cancellation: &AtomicBool,
) -> anyhow::Result<GitOutput> {
    git_paths_cancellable(root, prefix, &[path], Some(cancellation))
}

pub(super) fn git_paths_cancellable(
    root: &str,
    prefix: &[&[u8]],
    paths: &[&[u8]],
    cancellation: Option<&AtomicBool>,
) -> anyhow::Result<GitOutput> {
    for path in paths {
        validate_git_path(path)?;
    }
    let mut args: Vec<OsString> = prefix
        .iter()
        .map(|value| OsString::from_vec(value.to_vec()))
        .collect();
    args.push(OsString::from("--"));
    args.extend(paths.iter().map(|path| OsString::from_vec(path.to_vec())));
    let refs: Vec<_> = args.iter().map(OsString::as_os_str).collect();
    git_output_with_deadline(root, &refs, None, cancellation, GIT_MUTATION_DEADLINE)
}

pub(super) fn git_stdout_cancellable(
    root: &str,
    args: &[&OsStr],
    cancellation: Option<&AtomicBool>,
) -> anyhow::Result<Vec<u8>> {
    let output = git_output_cancellable(root, args, None, cancellation)?;
    ensure_success(&output, "Git command")?;
    Ok(output.output.stdout)
}

pub(super) fn git_index_generation(root: &str) -> anyhow::Result<String> {
    let output = git_output_cancellable(
        root,
        &[
            OsStr::new("ls-files"),
            OsStr::new("--stage"),
            OsStr::new("-z"),
        ],
        None,
        None,
    )?;
    ensure_success(&output, "read Git index authority")?;
    if output.stdout_truncated {
        bail!("Git index authority exceeds the bounded inspection limit");
    }
    Ok(blake3::hash(&output.stdout).to_hex().to_string())
}

pub(super) fn git_output_with_deadline(
    root: &str,
    args: &[&OsStr],
    stdin: Option<&[u8]>,
    cancellation: Option<&AtomicBool>,
    deadline: Duration,
) -> anyhow::Result<GitOutput> {
    git_output_inner(root, args, stdin, cancellation, deadline)
}

pub(super) fn git_output_cancellable(
    root: &str,
    args: &[&OsStr],
    stdin: Option<&[u8]>,
    cancellation: Option<&AtomicBool>,
) -> anyhow::Result<GitOutput> {
    git_output_inner(root, args, stdin, cancellation, GIT_DEADLINE)
}

fn git_output_inner(
    root: &str,
    args: &[&OsStr],
    stdin: Option<&[u8]>,
    cancellation: Option<&AtomicBool>,
    deadline: Duration,
) -> anyhow::Result<GitOutput> {
    let mut command = Command::new("git");
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    let worktree_only_apply =
        args.first() == Some(&OsStr::new("apply")) && !args.contains(&OsStr::new("--cached"));
    command.args(["-c", "core.quotePath=false"]);
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    let root_fd = root
        .strip_prefix("/dev/fd/")
        .and_then(|value| value.parse::<i32>().ok());
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    let root_fd: Option<i32> = None;
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    if root_fd.is_none() {
        command.arg("-C").arg(root);
    }
    command
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "true")
        .env("SSH_ASKPASS", "true")
        .env("GCM_INTERACTIVE", "never")
        // This applies to every Git subprocess, including commands whose path
        // arguments are assembled outside `git_path(s)`.
        .env("GIT_LITERAL_PATHSPECS", "1")
        .env("LC_ALL", "C")
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let metadata_binding = GIT_METADATA_ENV.with(|environment| {
        let binding = environment.borrow().last().cloned();
        #[cfg(target_os = "linux")]
        if let Some(binding) = &binding {
            command
                .env("GIT_DIR", &binding.git_dir)
                .env("GIT_COMMON_DIR", &binding.common_dir);
        }
        binding
    });
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    let _ = metadata_binding;
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    let darwin_boundary = DarwinGitBoundary::capture(
        root,
        root_fd,
        if worktree_only_apply {
            None
        } else {
            metadata_binding.as_ref()
        },
    )?;
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        command.arg("-C").arg(&darwin_boundary.root.path);
        if let Some(metadata) = &darwin_boundary.metadata {
            command
                .env("GIT_DIR", &metadata.git.path)
                .env("GIT_COMMON_DIR", &metadata.common.path)
                .env("GIT_WORK_TREE", &darwin_boundary.root.path);
        }
        command.args(args);
    }
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    command.args(args);
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    let pre_exec_boundary = darwin_boundary.clone();
    // SAFETY: this closure only performs async-signal-safe descriptor/path
    // validation and `setpgid` before exec. A separate process group lets
    // timeout/cancellation terminate hooks and grandchildren as well as Git.
    unsafe {
        command.pre_exec(move || {
            if libc::setpgid(0, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            pre_exec_boundary.validate_raw()?;
            Ok(())
        });
    }
    let mut child = command.spawn().context("failed to start Git")?;
    #[cfg(test)]
    let _phase14_process =
        super::phase14_git_process_started(args.first().copied().unwrap_or(OsStr::new("")));
    let stdout = child.stdout.take().context("Git stdout unavailable")?;
    let stderr = child.stderr.take().context("Git stderr unavailable")?;
    let stdout_reader = std::thread::spawn(move || read_process_output(stdout));
    let stderr_reader = std::thread::spawn(move || read_process_output(stderr));
    let stdin_writer = stdin.map(|input| {
        let mut pipe = child.stdin.take().expect("piped Git stdin is available");
        let input = input.to_vec();
        std::thread::spawn(move || pipe.write_all(&input))
    });
    let started = Instant::now();
    let termination = loop {
        if let Some(status) = child.try_wait()? {
            break (status, None);
        }
        if cancellation.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            break (
                terminate_child_group(&mut child)?,
                Some("Git command cancelled"),
            );
        }
        if started.elapsed() >= deadline {
            break (
                terminate_child_group(&mut child)?,
                Some("Git command timed out"),
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let (mut stdout, mut stdout_overflow) = stdout_reader
        .join()
        .map_err(|_| anyhow::anyhow!("Git stdout reader panicked"))??;
    let (mut stderr, mut stderr_overflow) = stderr_reader
        .join()
        .map_err(|_| anyhow::anyhow!("Git stderr reader panicked"))??;
    let stdin_error = stdin_writer
        .map(|writer| {
            writer
                .join()
                .map_err(|_| anyhow::anyhow!("Git stdin writer panicked"))?
                .map_err(anyhow::Error::from)
        })
        .transpose();
    let combined = stdout.len().saturating_add(stderr.len());
    if combined > MAX_GIT_OUTPUT {
        let mut excess = combined - MAX_GIT_OUTPUT;
        let trim_stderr = excess.min(stderr.len());
        stderr.truncate(stderr.len() - trim_stderr);
        stderr_overflow |= trim_stderr > 0;
        excess -= trim_stderr;
        if excess > 0 {
            stdout.truncate(stdout.len() - excess.min(stdout.len()));
            stdout_overflow = true;
        }
    }
    stdin_error?;
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    darwin_boundary
        .validate()
        .context("Git repository paths changed during command; refresh required")?;
    Ok(GitOutput {
        output: Output {
            status: termination.0,
            stdout,
            stderr,
        },
        stdout_truncated: stdout_overflow,
        stderr_truncated: stderr_overflow,
        interrupted: termination.1.map(str::to_owned),
    })
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
#[derive(Clone)]
struct DarwinDirectoryBinding {
    path: PathBuf,
    raw_path: CString,
    identity: (u64, u64),
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
#[derive(Clone)]
struct DarwinMetadataBinding {
    git: DarwinDirectoryBinding,
    common: DarwinDirectoryBinding,
}

// Match the path-based trust boundary used by mainstream desktop Git
// integrations: present stock Git with ordinary absolute paths, but bind those
// paths to the directories captured for this operation immediately before
// exec and again after completion. A hostile process running as the same user
// can still race Git's own pathname opens after exec; that is outside the local
// desktop threat model and would require a patched Git or privileged mediation.
#[cfg(any(target_os = "macos", target_os = "ios"))]
#[derive(Clone)]
struct DarwinGitBoundary {
    root: DarwinDirectoryBinding,
    metadata: Option<DarwinMetadataBinding>,
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
impl DarwinGitBoundary {
    fn capture(
        root: &str,
        root_fd: Option<i32>,
        metadata: Option<&GitMetadataBinding>,
    ) -> anyhow::Result<Self> {
        let root = match root_fd {
            Some(fd) => DarwinDirectoryBinding::capture(fd)?,
            None => DarwinDirectoryBinding::capture_path(root)?,
        };
        let metadata = metadata
            .map(|binding| {
                Ok::<_, anyhow::Error>(DarwinMetadataBinding {
                    git: DarwinDirectoryBinding::capture(binding.git_fd)?,
                    common: DarwinDirectoryBinding::capture(binding.common_fd)?,
                })
            })
            .transpose()?;
        let boundary = Self { root, metadata };
        boundary.validate()?;
        Ok(boundary)
    }

    fn validate(&self) -> anyhow::Result<()> {
        self.validate_raw().map_err(anyhow::Error::from)
    }

    fn validate_raw(&self) -> std::io::Result<()> {
        self.root.validate_raw()?;
        if let Some(metadata) = &self.metadata {
            metadata.git.validate_raw()?;
            metadata.common.validate_raw()?;
        }
        Ok(())
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
impl DarwinDirectoryBinding {
    fn capture(fd: i32) -> anyhow::Result<Self> {
        let path = descriptor_current_path(fd)?;
        let raw_path = CString::new(path.as_os_str().as_bytes())
            .context("Git repository path contains NUL")?;
        Ok(Self {
            path,
            raw_path,
            identity: descriptor_identity(fd)?,
        })
    }

    fn capture_path(path: &str) -> anyhow::Result<Self> {
        let raw_path = CString::new(path.as_bytes()).context("Git repository path contains NUL")?;
        // SAFETY: the path is live and a successful open returns an owned fd.
        let fd = unsafe {
            libc::open(
                raw_path.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error())
                .context("Git worktree path is unavailable");
        }
        let captured = Self::capture(fd);
        // SAFETY: the successful open returned this uniquely owned fd.
        unsafe { libc::close(fd) };
        captured
    }

    fn validate_raw(&self) -> std::io::Result<()> {
        // SAFETY: raw_path is a live C string. The fd is closed before return.
        let fd = unsafe {
            libc::open(
                self.raw_path.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let actual = descriptor_identity(fd);
        // SAFETY: the successful open returned this uniquely owned fd.
        unsafe { libc::close(fd) };
        if actual? != self.identity {
            return Err(std::io::Error::from_raw_os_error(libc::ESTALE));
        }
        Ok(())
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn descriptor_current_path(fd: i32) -> anyhow::Result<PathBuf> {
    let mut buffer = [0_u8; libc::PATH_MAX as usize];
    if unsafe { libc::fcntl(fd, libc::F_GETPATH, buffer.as_mut_ptr()) } < 0 {
        return Err(std::io::Error::last_os_error()).context("descriptor path unavailable");
    }
    let length = buffer
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(buffer.len());
    Ok(PathBuf::from(OsString::from_vec(buffer[..length].to_vec())))
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn descriptor_identity(fd: i32) -> std::io::Result<(u64, u64)> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: fd is live and the output buffer is valid for a complete stat.
    if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: successful fstat initialized the value.
    let stat = unsafe { stat.assume_init() };
    Ok((stat.st_dev as u64, stat.st_ino))
}

fn terminate_child_group(child: &mut Child) -> std::io::Result<std::process::ExitStatus> {
    let group = -(child.id() as i32);
    // SAFETY: negative pid addresses the child process group created in
    // `pre_exec`; ESRCH simply means the child exited between checks.
    let term = unsafe { libc::kill(group, libc::SIGTERM) };
    if term < 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error);
        }
    }
    let deadline = Instant::now() + TERMINATION_GRACE;
    let mut child_status = None;
    while Instant::now() < deadline {
        if child_status.is_none() {
            child_status = child.try_wait()?;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    // The direct Git process can exit on TERM while a hook/grandchild that
    // inherited its output pipes ignores TERM. Always finish by killing the
    // complete process group; otherwise joining the pipe readers can block
    // until that grandchild exits naturally.
    // SAFETY: same process-group contract as the TERM above.
    let kill = unsafe { libc::kill(group, libc::SIGKILL) };
    if kill < 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error);
        }
    }
    match child_status {
        Some(status) => Ok(status),
        None => child.wait(),
    }
}

fn read_process_output(mut reader: impl Read) -> std::io::Result<(Vec<u8>, bool)> {
    let mut stored = Vec::new();
    let mut overflow = false;
    let mut chunk = [0_u8; 64 * 1024];
    loop {
        let read = reader.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        let available = MAX_GIT_OUTPUT.saturating_sub(stored.len());
        stored.extend_from_slice(&chunk[..read.min(available)]);
        overflow |= read > available;
    }
    Ok((stored, overflow))
}

pub(super) fn ensure_success(output: &GitOutput, action: &str) -> anyhow::Result<()> {
    if let Some(interrupted) = &output.interrupted {
        bail!(
            "{interrupted}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
    } else if output.status.success() {
        Ok(())
    } else {
        bail!(
            "{action} failed ({}): {}",
            output
                .status
                .code()
                .map_or_else(|| "signal".into(), |value| value.to_string()),
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }
}

#[cfg(test)]
pub(super) fn command_result(
    output: impl Into<GitOutput>,
    refresh: anyhow::Result<v1::GitStatusSnapshot>,
) -> v1::GitCommandResult {
    let output = output.into();
    let applied = output.status.success();
    let (status, refresh_failed, refresh_error) = match refresh {
        Ok(status) => (Some(status), false, String::new()),
        Err(error) => (None, true, error.to_string()),
    };
    v1::GitCommandResult {
        exit_code: output.status.code().unwrap_or(-1),
        stdout: output.output.stdout,
        stderr: output.output.stderr,
        status,
        applied,
        refresh_failed,
        refresh_error,
        outcome: if applied {
            v1::GitCommandOutcome::Applied.into()
        } else {
            v1::GitCommandOutcome::NotApplied.into()
        },
        stdout_truncated: output.stdout_truncated,
        stderr_truncated: output.stderr_truncated,
        error: output.interrupted.unwrap_or_default(),
        ..Default::default()
    }
}

pub(super) fn success_output() -> GitOutput {
    use std::os::unix::process::ExitStatusExt;
    Output {
        status: std::process::ExitStatus::from_raw(0),
        stdout: Vec::new(),
        stderr: Vec::new(),
    }
    .into()
}
