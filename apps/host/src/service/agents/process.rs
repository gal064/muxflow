use std::collections::{BTreeSet, VecDeque};
#[cfg(any(test, target_os = "linux"))]
use std::{fs, path::Path};

use super::adapters;

const MAX_PROCESSES: usize = 32;
const MAX_DEPTH: usize = 4;
const MAX_ARGV_BYTES: usize = 16 * 1024;
const MAX_ANCESTORS: usize = 16;

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

/// Re-checks the process tree without trusting tmux's cached command fields.
/// Voice uses this immediately before delivery so an exited agent cannot turn
/// a pending utterance into input for the surviving shell.
pub(super) fn detect_live(
    pane: &tmux_control::Pane,
) -> Option<&'static dyn adapters::AgentAdapter> {
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
        let _ = pane;
        None
    }
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
        if let Some(argv) = proc_argv(proc_root, pid)
            && let Some(adapter) = adapters::detect_argv(&argv)
        {
            return Some(adapter);
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

#[cfg(any(test, target_os = "linux"))]
fn proc_argv(proc_root: &Path, pid: u32) -> Option<Vec<String>> {
    let bytes = fs::read(proc_root.join(pid.to_string()).join("cmdline")).ok()?;
    Some(
        bytes[..bytes.len().min(MAX_ARGV_BYTES)]
            .split(|byte| *byte == 0)
            .filter(|arg| !arg.is_empty())
            .map(|arg| String::from_utf8_lossy(arg).into_owned())
            .collect(),
    )
}

/// Whether the agent that spawned this process was itself started, through a
/// shell or another tool, by a second agent.
///
/// A nested agent inherits `TMUX_PANE`, so its hooks would otherwise report its
/// own turn as the state of the agent that owns the pane. Session IDs cannot
/// tell the two apart: a nested run and a real `/branch` or new session look the
/// same in the payload. An unreadable chain answers false, leaving the hook to
/// report as usual.
pub(crate) fn runs_beneath_another_agent() -> bool {
    #[cfg(target_os = "linux")]
    {
        beneath_another_agent(proc_ancestors(Path::new("/proc"), std::process::id()))
    }
    #[cfg(target_os = "macos")]
    {
        beneath_another_agent(darwin_ancestors(std::process::id()))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        false
    }
}

/// `ancestors` runs nearest first. A same-adapter process directly above the
/// emitter is its own launcher or shim, so that adapter only counts as a second
/// agent once a non-agent process separates them. A different adapter is always
/// a second agent, even when adjacent: `bash -c` with a single command replaces
/// itself, leaving no shell between an agent and what it launched.
#[cfg(any(test, target_os = "linux", target_os = "macos"))]
fn beneath_another_agent(ancestors: Vec<Vec<String>>) -> bool {
    let mut emitter = None;
    let mut left_emitter = false;
    for argv in ancestors {
        let agent = adapters::detect_argv(&argv).map(|adapter| adapter.id());
        if let (Some(agent), Some(emitter)) = (agent, emitter)
            && (left_emitter || agent != emitter)
        {
            return true;
        }
        emitter = emitter.or(agent);
        left_emitter |= emitter.is_some() && agent.is_none();
    }
    false
}

#[cfg(any(test, target_os = "linux"))]
fn proc_ancestors(proc_root: &Path, start_pid: u32) -> Vec<Vec<String>> {
    let mut chain = Vec::new();
    let mut pid = start_pid;
    while pid > 1 && chain.len() < MAX_ANCESTORS {
        let Some(argv) = proc_argv(proc_root, pid) else {
            break;
        };
        chain.push(argv);
        let Some(parent) = proc_parent_pid(proc_root, pid) else {
            break;
        };
        pid = parent;
    }
    chain
}

/// The parent is the second field after `comm`, which is parenthesized and may
/// itself contain spaces and `)`.
#[cfg(any(test, target_os = "linux"))]
fn proc_parent_pid(proc_root: &Path, pid: u32) -> Option<u32> {
    let bytes = fs::read(proc_root.join(pid.to_string()).join("stat")).ok()?;
    let stat = String::from_utf8_lossy(&bytes);
    let (_, after_comm) = stat.rsplit_once(')')?;
    after_comm.split_ascii_whitespace().nth(1)?.parse().ok()
}

#[cfg(target_os = "macos")]
fn darwin_ancestors(start_pid: u32) -> Vec<Vec<String>> {
    let mut chain = Vec::new();
    let mut pid = start_pid;
    while pid > 1 && chain.len() < MAX_ANCESTORS {
        let Some(argv) = darwin_process_argv(pid) else {
            break;
        };
        chain.push(argv);
        let Some(parent) = darwin_parent_pid(pid) else {
            break;
        };
        pid = parent;
    }
    chain
}

#[cfg(target_os = "macos")]
fn darwin_parent_pid(pid: u32) -> Option<u32> {
    let pid = libc::c_int::try_from(pid).ok()?;
    let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
    // SAFETY: an all-zero `proc_bsdinfo` is a valid value of this plain C struct.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    // SAFETY: `info` owns `size` writable bytes for this read-only query.
    let written =
        unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size) };
    (written == size).then_some(info.pbi_ppid)
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

    /// Writes a fake `/proc` chain: each entry's parent is the next entry.
    fn fake_chain(argvs: &[&[u8]]) -> std::path::PathBuf {
        let root = std::env::current_dir()
            .unwrap()
            .join("tmp")
            .join(format!("nested-proc-{}", uuid::Uuid::new_v4()));
        for (index, argv) in argvs.iter().enumerate() {
            let pid = 10 + index;
            fs::create_dir_all(root.join(pid.to_string())).unwrap();
            fs::write(root.join(format!("{pid}/cmdline")), argv).unwrap();
            fs::write(
                root.join(format!("{pid}/stat")),
                format!("{pid} (proc) S {} 0 0", pid + 1),
            )
            .unwrap();
        }
        root
    }

    fn chain_is_nested(argvs: &[&[u8]]) -> bool {
        let root = fake_chain(argvs);
        let nested = beneath_another_agent(proc_ancestors(&root, 10));
        fs::remove_dir_all(root).unwrap();
        nested
    }

    const HOOK: &[u8] = b"/home/u/.local/bin/muxflow-host\0hook\0ingest\0";
    const CLAUDE: &[u8] = b"claude\0-p\0ok\0";
    const NODE_CLAUDE: &[u8] = b"node\0/usr/lib/claude-code/cli.js\0";
    const SHELL: &[u8] = b"/usr/bin/bash\0";

    #[test]
    fn an_agent_started_from_another_agents_shell_is_nested() {
        assert!(chain_is_nested(&[HOOK, CLAUDE, SHELL, CLAUDE, SHELL]));
        assert!(chain_is_nested(&[HOOK, CLAUDE, SHELL, NODE_CLAUDE, SHELL]));
        assert!(chain_is_nested(&[
            HOOK,
            CLAUDE,
            b"timeout\08\0claude\0",
            SHELL,
            b"codex\0",
            SHELL,
        ]));
    }

    #[test]
    fn a_top_level_agent_is_not_nested() {
        assert!(!chain_is_nested(&[
            HOOK,
            b"/bin/sh\0-c\0hook\0",
            CLAUDE,
            SHELL,
            b"tmux: server\0",
        ]));
    }

    #[test]
    fn a_launcher_directly_above_the_agent_is_not_a_second_agent() {
        assert!(!chain_is_nested(&[HOOK, NODE_CLAUDE, CLAUDE, SHELL]));
    }

    #[test]
    fn a_different_agent_directly_above_is_a_second_agent() {
        assert!(chain_is_nested(&[HOOK, CLAUDE, b"pi\0", SHELL]));
        assert!(chain_is_nested(&[HOOK, CLAUDE, b"codex\0", SHELL]));
    }

    #[test]
    fn a_shell_whose_script_mentions_an_agent_is_not_an_agent() {
        assert!(!chain_is_nested(&[
            HOOK,
            CLAUDE,
            b"/usr/bin/bash\0-c\0claude -p ok\0",
            SHELL,
        ]));
    }

    #[test]
    fn an_unreadable_chain_is_not_nested() {
        let root = fake_chain(&[HOOK, CLAUDE, SHELL, CLAUDE]);
        fs::remove_dir_all(root.join("12")).unwrap();
        assert!(!beneath_another_agent(proc_ancestors(&root, 10)));
        fs::write(root.join("11/stat"), b"11 (claude no close paren").unwrap();
        assert!(!beneath_another_agent(proc_ancestors(&root, 10)));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn parent_pid_survives_a_command_name_with_spaces_and_parens() {
        let root = fake_chain(&[HOOK]);
        fs::write(root.join("10/stat"), b"10 (we ird) (x) S 4242 0 0").unwrap();
        assert_eq!(proc_parent_pid(&root, 10), Some(4242));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_ancestor_walk_is_bounded() {
        let root = fake_chain(&vec![SHELL; MAX_ANCESTORS + 4]);
        assert_eq!(proc_ancestors(&root, 10).len(), MAX_ANCESTORS);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn darwin_parent_pid_matches_the_kernel() {
        let parent = u32::try_from(unsafe { libc::getppid() }).unwrap();
        assert_eq!(darwin_parent_pid(std::process::id()), Some(parent));
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
