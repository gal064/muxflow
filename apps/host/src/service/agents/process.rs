use std::{
    collections::{BTreeSet, VecDeque},
    fs,
    path::Path,
};

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
            #[cfg(not(target_os = "linux"))]
            {
                None
            }
        })
}

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
}
