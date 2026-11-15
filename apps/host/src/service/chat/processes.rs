//! What a chat reads of a running process to replace it: its command line,
//! its children, how long it has run, its environment and its folder. Linux
//! keeps them in `/proc`; macOS is asked for them.

use std::{io, path::PathBuf, time::Duration};

pub(super) const MAX_ARGV_BYTES: usize = 64 * 1024;
pub(super) const MAX_ENVIRONMENT_BYTES: usize = 4 * 1024 * 1024;

pub(super) trait Processes {
    /// A whole command line, or nothing: one that was cut short may have
    /// lost the argument that says what the agent was asked to do.
    fn argv(&self, pid: u32) -> Option<Vec<String>>;
    fn children(&self, pid: u32) -> Vec<u32>;
    /// How long ago it started, when that could be read.
    fn age(&self, pid: u32) -> Option<Duration>;
    /// Stable kernel start stamp, to distinguish reuse of a PID.
    fn start_stamp(&self, pid: u32) -> Option<u64>;
    /// Its variables as `/proc/<pid>/environ` holds them: each one
    /// `NAME=value` and a zero byte.
    fn environment(&self, pid: u32) -> io::Result<Vec<u8>>;
    fn folder(&self, pid: u32) -> io::Result<PathBuf>;
}

/// The processes of this machine.
#[cfg(not(target_os = "macos"))]
pub(super) fn system() -> impl Processes {
    Proc(std::path::Path::new("/proc"))
}

#[cfg(target_os = "macos")]
pub(super) fn system() -> impl Processes {
    Darwin
}

/// A `/proc`, or a folder laid out as one.
#[cfg(any(test, not(target_os = "macos")))]
pub(super) struct Proc<'a>(pub(super) &'a std::path::Path);

#[cfg(any(test, not(target_os = "macos")))]
impl Processes for Proc<'_> {
    fn argv(&self, pid: u32) -> Option<Vec<String>> {
        let bytes = std::fs::read(self.0.join(pid.to_string()).join("cmdline")).ok()?;
        if bytes.is_empty() || bytes.len() > MAX_ARGV_BYTES {
            return None;
        }
        bytes
            .strip_suffix(&[0])
            .unwrap_or(&bytes)
            .split(|byte| *byte == 0)
            .map(|argument| String::from_utf8(argument.to_vec()).ok())
            .collect()
    }

    fn children(&self, pid: u32) -> Vec<u32> {
        let pid = pid.to_string();
        let children = self.0.join(&pid).join("task").join(&pid).join("children");
        std::fs::read_to_string(children)
            .map(|children| {
                children
                    .split_ascii_whitespace()
                    .filter_map(|pid| pid.parse().ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn age(&self, pid: u32) -> Option<Duration> {
        let start_ticks = self.start_stamp(pid)? as f64;
        let uptime = std::fs::read_to_string(self.0.join("uptime")).ok()?;
        let uptime: f64 = uptime.split_ascii_whitespace().next()?.parse().ok()?;
        // SAFETY: `sysconf` reads a constant of the running system.
        let ticks_per_second = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        if ticks_per_second <= 0 {
            return None;
        }
        let age = uptime - start_ticks / ticks_per_second as f64;
        Some(Duration::from_secs_f64(age.max(0.0)))
    }

    fn start_stamp(&self, pid: u32) -> Option<u64> {
        let stat = std::fs::read_to_string(self.0.join(pid.to_string()).join("stat")).ok()?;
        // The command name can contain spaces and parentheses.
        stat[stat.rfind(')')? + 1..]
            .split_ascii_whitespace()
            .nth(19)?
            .parse()
            .ok()
    }

    fn environment(&self, pid: u32) -> io::Result<Vec<u8>> {
        std::fs::read(self.0.join(pid.to_string()).join("environ"))
    }

    fn folder(&self, pid: u32) -> io::Result<PathBuf> {
        std::fs::read_link(self.0.join(pid.to_string()).join("cwd"))
    }
}

/// What the kernel keeps of how a process was started (`KERN_PROCARGS2`), as
/// its command line and its environment: the argument count, the program's
/// path, zero bytes up to the first argument, the arguments and then the
/// variables, each ended by a zero byte. `None` when it was cut short.
#[cfg(any(test, target_os = "macos"))]
fn started_with(bytes: &[u8]) -> Option<(Vec<String>, Vec<u8>)> {
    let count = i32::from_ne_bytes(bytes.get(..4)?.try_into().ok()?);
    let count = usize::try_from(count).ok().filter(|count| *count > 0)?;
    let rest = &bytes[4..];
    let path_end = rest.iter().position(|byte| *byte == 0)?;
    let rest = &rest[path_end..];
    let rest = &rest[rest.iter().position(|byte| *byte != 0)?..];
    let mut strings = rest.split(|byte| *byte == 0);
    let mut argv = Vec::new();
    let mut argv_bytes = 0;
    for _ in 0..count {
        let argument = strings.next()?;
        argv_bytes += argument.len() + 1;
        if argv_bytes > MAX_ARGV_BYTES {
            return None;
        }
        argv.push(String::from_utf8(argument.to_vec()).ok()?);
    }
    // The last piece is what follows the last zero byte: nothing, when the
    // whole of it was read.
    let mut rest: Vec<&[u8]> = strings.collect();
    if !rest.pop()?.is_empty() {
        return None;
    }
    let mut environment = Vec::new();
    for variable in rest.into_iter().take_while(|variable| !variable.is_empty()) {
        environment.extend_from_slice(variable);
        environment.push(0);
    }
    Some((argv, environment))
}

#[cfg(target_os = "macos")]
struct Darwin;

#[cfg(target_os = "macos")]
impl Darwin {
    fn started_with(pid: u32) -> Option<(Vec<String>, Vec<u8>)> {
        let pid = i32::try_from(pid).ok()?;
        let mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
        let mut size = 0usize;
        // SAFETY: a sizing query: a valid MIB and no output buffer.
        let sized = unsafe {
            libc::sysctl(
                mib.as_ptr().cast_mut(),
                mib.len() as u32,
                std::ptr::null_mut(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if sized != 0 || size > MAX_ARGV_BYTES + MAX_ENVIRONMENT_BYTES {
            return None;
        }
        let mut bytes = vec![0u8; size];
        // SAFETY: `bytes` owns `size` writable bytes for the same query.
        let read = unsafe {
            libc::sysctl(
                mib.as_ptr().cast_mut(),
                mib.len() as u32,
                bytes.as_mut_ptr().cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if read != 0 {
            return None;
        }
        bytes.truncate(size);
        started_with(&bytes)
    }

    fn info(pid: u32) -> Option<libc::proc_bsdinfo> {
        let pid = libc::c_int::try_from(pid).ok()?;
        let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
        // SAFETY: an all-zero `proc_bsdinfo` is a valid value of this plain
        // C struct.
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        // SAFETY: `info` owns `size` writable bytes for this read-only query.
        let written = unsafe {
            libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size)
        };
        (written == size).then_some(info)
    }
}

#[cfg(target_os = "macos")]
impl Processes for Darwin {
    fn start_stamp(&self, pid: u32) -> Option<u64> {
        let info = Self::info(pid)?;
        Some(info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec)
    }
    fn argv(&self, pid: u32) -> Option<Vec<String>> {
        Self::started_with(pid).map(|(argv, _)| argv)
    }

    fn children(&self, pid: u32) -> Vec<u32> {
        const MAX_CHILDREN: usize = 256;
        let Ok(pid) = libc::pid_t::try_from(pid) else {
            return Vec::new();
        };
        let mut pids = [0 as libc::pid_t; MAX_CHILDREN];
        let Ok(bytes) = libc::c_int::try_from(std::mem::size_of_val(&pids)) else {
            return Vec::new();
        };
        // SAFETY: `pids` owns `bytes` writable bytes for this bounded query.
        let count = unsafe { libc::proc_listchildpids(pid, pids.as_mut_ptr().cast(), bytes) };
        // What it answers with is how many there are, not how many bytes.
        let count = usize::try_from(count).unwrap_or_default().min(MAX_CHILDREN);
        pids[..count]
            .iter()
            .filter_map(|pid| u32::try_from(*pid).ok())
            .filter(|pid| *pid != 0)
            .collect()
    }

    fn age(&self, pid: u32) -> Option<Duration> {
        let info = Self::info(pid)?;
        let started = std::time::UNIX_EPOCH
            + Duration::new(info.pbi_start_tvsec, 0)
            + Duration::from_micros(info.pbi_start_tvusec);
        Some(
            std::time::SystemTime::now()
                .duration_since(started)
                .unwrap_or_default(),
        )
    }

    fn environment(&self, pid: u32) -> io::Result<Vec<u8>> {
        Self::started_with(pid)
            .map(|(_, environment)| environment)
            .ok_or_else(|| io::Error::other("the process's environment could not be read"))
    }

    fn folder(&self, pid: u32) -> io::Result<PathBuf> {
        use std::os::unix::ffi::OsStringExt;

        let unreadable = || io::Error::other("the process's folder could not be read");
        let pid = libc::c_int::try_from(pid).map_err(|_| unreadable())?;
        let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_vnodepathinfo>())
            .map_err(|_| unreadable())?;
        // SAFETY: an all-zero `proc_vnodepathinfo` is a valid value of this
        // plain C struct.
        let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
        // SAFETY: `info` owns `size` writable bytes for this read-only query.
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDVNODEPATHINFO,
                0,
                (&raw mut info).cast(),
                size,
            )
        };
        if written != size {
            return Err(unreadable());
        }
        let path: Vec<u8> = info
            .pvi_cdir
            .vip_path
            .as_flattened()
            .iter()
            .take_while(|byte| **byte != 0)
            .map(|byte| byte.cast_unsigned())
            .collect();
        if path.is_empty() {
            return Err(unreadable());
        }
        Ok(PathBuf::from(std::ffi::OsString::from_vec(path)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kept(count: i32, after: &[u8]) -> Vec<u8> {
        let mut bytes = count.to_ne_bytes().to_vec();
        bytes.extend_from_slice(b"/usr/local/bin/claude\0\0\0\0");
        bytes.extend_from_slice(after);
        bytes
    }

    #[test]
    fn what_the_kernel_keeps_reads_as_the_command_line_and_the_environment() {
        let bytes = kept(
            3,
            b"claude\0--resume\0\0HOME=/Users/dev\0EMPTY=\0PATH=/bin\0\0\0\0",
        );
        let (argv, environment) = started_with(&bytes).unwrap();
        // An empty argument is one, and is not where the arguments end.
        assert_eq!(argv, ["claude", "--resume", ""]);
        assert_eq!(environment, b"HOME=/Users/dev\0EMPTY=\0PATH=/bin\0");
        assert_eq!(
            crate::chat_pipe::parse_environment(&environment).len(),
            3,
            "read as the pane's helper reads it"
        );

        let (argv, environment) = started_with(&kept(1, b"codex\0")).unwrap();
        assert_eq!(argv, ["codex"]);
        assert!(environment.is_empty());
    }

    #[test]
    fn a_command_line_that_was_cut_short_is_not_one() {
        assert!(started_with(&kept(3, b"claude\0--resume\0")).is_none());
        assert!(started_with(&kept(2, b"claude\0--resu")).is_none());
        assert!(started_with(&kept(1, b"claude\0HOME=/Users/d")).is_none());
        assert!(started_with(&kept(0, b"claude\0")).is_none());
        assert!(started_with(&kept(1, b"\xff\0")).is_none());
        assert!(started_with(&[1, 0]).is_none());
        let long = vec![b'a'; MAX_ARGV_BYTES];
        assert!(started_with(&kept(1, &[&long[..], b"\0"].concat())).is_none());
    }

    /// On a Mac: a real process, started with known arguments, variables and
    /// folder, reads back as exactly those.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_running_process_reads_as_it_was_started() {
        let folder = tempfile::tempdir().unwrap();
        let executable = std::env::current_exe().unwrap();
        let arguments = [
            "--exact",
            "service::chat::processes::tests::process_fixture",
            "--ignored",
        ];
        let child = std::process::Command::new(&executable)
            .args(arguments)
            .env_clear()
            .env("A", "1")
            .env("EMPTY", "")
            .env("PATH", "/usr/bin:/bin")
            .current_dir(folder.path())
            .spawn()
            .unwrap();
        struct Child(std::process::Child);
        impl Drop for Child {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let child = Child(child);
        let pid = child.0.id();
        // Between the fork and the exec it is still this process's copy.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !folder.path().join("ready").exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let read = (
            Darwin.argv(pid),
            Darwin
                .environment(pid)
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .map_err(|error| error.to_string()),
            Darwin.folder(pid).map_err(|error| error.to_string()),
            Darwin.age(pid),
            Darwin.children(std::process::id()),
        );
        assert!(
            folder.path().join("ready").exists(),
            "fixture did not start"
        );
        drop(child);
        let (argv, environment, started_in, age, children) = read;
        let expected: Vec<String> = std::iter::once(executable.to_string_lossy().into_owned())
            .chain(arguments.map(str::to_owned))
            .collect();
        assert_eq!(argv.unwrap(), expected);
        assert_eq!(environment.unwrap(), "A=1\0EMPTY=\0PATH=/usr/bin:/bin\0");
        assert_eq!(started_in.unwrap(), folder.path().canonicalize().unwrap());
        assert!(age.unwrap() < Duration::from_secs(30), "{age:?}");
        assert!(children.contains(&pid), "{children:?}");
    }

    // A system executable can hide its environment on macOS. Spawn this
    // test binary instead, without depending on a separate compiler or tool.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "spawned by a_running_process_reads_as_it_was_started"]
    fn process_fixture() {
        assert_eq!(std::env::var("A").as_deref(), Ok("1"));
        std::fs::write("ready", "").unwrap();
        std::thread::sleep(Duration::from_secs(30));
    }
}
