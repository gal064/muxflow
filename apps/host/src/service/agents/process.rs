use std::collections::{BTreeSet, VecDeque};
#[cfg(any(test, target_os = "linux"))]
use std::{fs, path::Path};

use super::adapters;

const MAX_PROCESSES: usize = 32;
const MAX_DEPTH: usize = 4;
const MAX_ARGV_BYTES: usize = 16 * 1024;

pub(super) fn detect(pane: &tmux_control::Pane) -> Option<&'static dyn adapters::AgentAdapter> {
    adapters::detect(&pane.current_command)
        .or_else(|| {
            let argv = pane
                .start_command
                .split_ascii_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>();
            adapters::detect_argv(&argv)
        })
        .or_else(|| {
            #[cfg(target_os = "linux")]
            {
                detect_proc_tree(Path::new("/proc"), pane.pane_pid)
            }
            #[cfg(target_os = "macos")]
            {
                detect_darwin_tree(pane.pane_pid)
            }
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            {
                None
            }
        })
}

#[cfg(any(test, target_os = "linux"))]
fn detect_proc_tree(
    proc_root: &Path,
    root_pid: u32,
) -> Option<&'static dyn adapters::AgentAdapter> {
    if root_pid == 0 {
        return None;
    }
    let mut queue = VecDeque::from([(root_pid, 0usize)]);
    let mut visited = BTreeSet::new();
    while let Some((pid, depth)) = queue.pop_front() {
        if visited.len() >= MAX_PROCESSES || !visited.insert(pid) {
            continue;
        }
        if let Ok(bytes) = fs::read(proc_root.join(pid.to_string()).join("cmdline")) {
            let argv = bytes[..bytes.len().min(MAX_ARGV_BYTES)]
                .split(|byte| *byte == 0)
                .filter(|arg| !arg.is_empty())
                .map(|arg| String::from_utf8_lossy(arg).into_owned())
                .collect::<Vec<_>>();
            if let Some(adapter) = adapters::detect_argv(&argv) {
                return Some(adapter);
            }
        }
        if depth >= MAX_DEPTH {
            continue;
        }
        let children = proc_root
            .join(pid.to_string())
            .join("task")
            .join(pid.to_string())
            .join("children");
        if let Ok(children) = fs::read_to_string(children) {
            queue.extend(
                children
                    .split_ascii_whitespace()
                    .filter_map(|pid| pid.parse().ok())
                    .map(|pid| (pid, depth + 1)),
            );
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn detect_darwin_tree(root_pid: u32) -> Option<&'static dyn adapters::AgentAdapter> {
    if root_pid == 0 {
        return None;
    }
    let mut queue = VecDeque::from([(root_pid, 0usize)]);
    let mut visited = BTreeSet::new();
    while let Some((pid, depth)) = queue.pop_front() {
        if visited.len() >= MAX_PROCESSES || !visited.insert(pid) {
            continue;
        }
        if let Some(argv) = darwin_process_argv(pid)
            && let Some(adapter) = adapters::detect_argv(&argv)
        {
            return Some(adapter);
        }
        if depth < MAX_DEPTH {
            queue.extend(
                darwin_child_pids(pid)
                    .into_iter()
                    .map(|child| (child, depth + 1)),
            );
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn darwin_child_pids(parent: u32) -> Vec<u32> {
    let Ok(parent) = libc::pid_t::try_from(parent) else {
        return Vec::new();
    };
    let element = std::mem::size_of::<libc::pid_t>();
    let capacity = MAX_PROCESSES.saturating_mul(element);
    let Ok(capacity_i32) = i32::try_from(capacity) else {
        return Vec::new();
    };
    let mut pids = vec![0 as libc::pid_t; MAX_PROCESSES];
    // SAFETY: `pids` owns `capacity` writable bytes for the bounded call.
    let count = unsafe { libc::proc_listchildpids(parent, pids.as_mut_ptr().cast(), capacity_i32) };
    if count <= 0 {
        return Vec::new();
    }
    // Unlike proc_listpids, proc_listchildpids returns a PID count rather than
    // a byte count. Dividing by sizeof(pid_t) silently discarded small child
    // sets, including the usual one-agent-under-one-shell topology.
    let count = usize::try_from(count).ok().unwrap_or_default();
    pids.into_iter()
        .take(count.min(MAX_PROCESSES))
        .filter_map(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid != 0)
        .collect()
}

#[cfg(target_os = "macos")]
fn darwin_process_argv(pid: u32) -> Option<Vec<String>> {
    let pid_i32 = i32::try_from(pid).ok()?;
    let mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid_i32];
    let mut size = 0usize;
    // SAFETY: this sizing query has a valid MIB and null output buffer.
    if unsafe {
        libc::sysctl(
            mib.as_ptr().cast_mut(),
            mib.len() as u32,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return None;
    }
    size = size.min(MAX_ARGV_BYTES);
    if size < std::mem::size_of::<libc::c_int>() {
        return None;
    }
    let mut bytes = vec![0u8; size];
    // SAFETY: `bytes` owns `size` writable bytes for the same read-only MIB.
    if unsafe {
        libc::sysctl(
            mib.as_ptr().cast_mut(),
            mib.len() as u32,
            bytes.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return None;
    }
    bytes.truncate(size);
    let argc = i32::from_ne_bytes(bytes[..4].try_into().ok()?);
    if argc <= 0 {
        return None;
    }
    let mut cursor = 4;
    while cursor < bytes.len() && bytes[cursor] != 0 {
        cursor += 1;
    }
    while cursor < bytes.len() && bytes[cursor] == 0 {
        cursor += 1;
    }
    let mut argv = Vec::with_capacity((argc as usize).min(32));
    for _ in 0..argc.min(32) {
        if cursor >= bytes.len() {
            break;
        }
        let end = bytes[cursor..]
            .iter()
            .position(|byte| *byte == 0)
            .map(|offset| cursor + offset)
            .unwrap_or(bytes.len());
        if end > cursor {
            argv.push(String::from_utf8_lossy(&bytes[cursor..end]).into_owned());
        }
        cursor = end.saturating_add(1);
    }
    (!argv.is_empty()).then_some(argv)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_tree_detects_node_wrapped_agent() {
        let root = std::env::current_dir()
            .unwrap()
            .join("tmp")
            .join(format!("phase6-proc-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("10/task/10")).unwrap();
        fs::create_dir_all(root.join("11/task/11")).unwrap();
        fs::write(root.join("10/cmdline"), b"bash\0").unwrap();
        fs::write(root.join("10/task/10/children"), b"11").unwrap();
        fs::write(
            root.join("11/cmdline"),
            b"node\0/usr/lib/claude-code/cli.js\0",
        )
        .unwrap();
        assert_eq!(detect_proc_tree(&root, 10).unwrap().id(), "claude-code");
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn darwin_process_argv_reads_the_current_process_within_the_bound() {
        let argv = darwin_process_argv(std::process::id()).expect("current argv");
        assert!(!argv.is_empty());
        assert!(argv.len() <= 32);
        assert!(argv.iter().map(String::len).sum::<usize>() <= MAX_ARGV_BYTES);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn darwin_process_tree_detects_a_child_by_argv_zero() {
        use std::{os::unix::process::CommandExt, process::Command};

        let mut child = Command::new("/bin/sleep")
            .arg0("codex")
            .arg("30")
            .spawn()
            .expect("spawn bounded codex fixture");
        let detected = detect_darwin_tree(child.id()).map(|adapter| adapter.id());
        child.kill().expect("stop bounded codex fixture");
        child.wait().expect("reap bounded codex fixture");

        assert_eq!(detected, Some("codex"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn darwin_process_tree_detects_a_shell_child() {
        use std::{process::Command, thread, time::Duration};

        let mut shell = Command::new("/bin/bash")
            .args(["-c", "exec -a codex /bin/sleep 30 & wait"])
            .spawn()
            .expect("spawn bounded shell fixture");
        let detected = (0..20).find_map(|_| {
            let detected = detect_darwin_tree(shell.id()).map(|adapter| adapter.id());
            if detected.is_none() {
                thread::sleep(Duration::from_millis(10));
            }
            detected
        });
        shell.kill().expect("stop bounded shell fixture");
        shell.wait().expect("reap bounded shell fixture");

        assert_eq!(detected, Some("codex"));
    }
}
