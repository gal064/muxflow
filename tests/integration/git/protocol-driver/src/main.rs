use std::{fs, os::unix::fs::PermissionsExt, process::Command};

use protocol_driver_support::{Bridge, Hello, local_bridge_command, ssh_bridge_command};
use tmux_agent_protocol::{HOST_CAPABILITIES, v1};
use uuid::Uuid;

struct Connection {
    bridge: Bridge,
    hello: v1::ServerHello,
    epoch: u64,
}

impl Connection {
    fn open(arguments: &[String], epoch: u64) -> Result<Self, String> {
        let mut command = bridge_command(arguments)?;
        let (bridge, hello) = Bridge::connect(
            &mut command,
            Hello {
                desktop_version: "phase5-driver",
                requested_capabilities: HOST_CAPABILITIES,
                bulk_connection: false,
                expected_server_identity: "",
                connection_epoch: epoch,
            },
            10,
        )?;
        Ok(Self {
            bridge,
            hello,
            epoch,
        })
    }

    fn request(&mut self, request: v1::Request) -> Result<v1::Response, String> {
        self.bridge.request(request)
    }

    fn request_error(&mut self, request: v1::Request) -> Result<v1::Response, String> {
        self.bridge.request_error(request)
    }

    fn send_without_response(&mut self, request: v1::Request) -> Result<(), String> {
        self.bridge.send(request).map(|_| ())
    }

    fn active_root(&mut self) -> Result<v1::ActiveRoot, String> {
        let snapshot = self
            .request(v1::Request {
                operation: v1::Operation::Subscribe.into(),
                scope: "full".into(),
                ..Default::default()
            })?
            .snapshot
            .ok_or("subscribe omitted snapshot")?;
        let pane = snapshot
            .panes
            .iter()
            .find(|pane| pane.active)
            .or_else(|| snapshot.panes.first())
            .ok_or_else(|| {
                format!(
                    "fixture has no pane (hello identity {:?}, snapshot identity {:?})",
                    self.hello.server_identity, snapshot.server_identity
                )
            })?;
        self.request(v1::Request {
            operation: v1::Operation::ResolveActiveRoot.into(),
            file: Some(v1::FileServiceRequest {
                operation_id: Uuid::new_v4().to_string(),
                pane_id: pane.id.clone(),
                expected_server_identity: snapshot.server_identity,
                expected_topology_generation: snapshot.generation,
                ..Default::default()
            }),
            ..Default::default()
        })?
        .file
        .and_then(|file| file.active_root)
        .ok_or("active-root response omitted payload".into())
    }
}

// The byte-exact path fixture. It is `raw-\xff` — deliberately not valid UTF-8
// — everywhere the repository lives on Linux, which is where this case must be
// proven. APFS enforces valid UTF-8 in filenames and rejects that name with
// EILSEQ, so the local macOS route cannot construct it at all and supplies a
// non-ASCII UTF-8 name instead. The invalid-UTF-8 case keeps its full coverage
// on the Docker/SSH route, whose repository is created inside the container.
fn raw_path_fixture() -> Vec<u8> {
    let Ok(encoded) = std::env::var("ADE_PHASE5_RAW_NAME_HEX") else {
        return b"raw-\xff".to_vec();
    };
    if encoded.is_empty() {
        return b"raw-\xff".to_vec();
    }
    assert!(
        encoded.len().is_multiple_of(2),
        "ADE_PHASE5_RAW_NAME_HEX must be an even-length hexadecimal string"
    );
    (0..encoded.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&encoded[index..index + 2], 16)
                .expect("ADE_PHASE5_RAW_NAME_HEX must be hexadecimal")
        })
        .collect()
}

fn main() -> Result<(), String> {
    let arguments: Vec<_> = std::env::args().collect();
    if !matches!(arguments.get(1).map(String::as_str), Some("local" | "ssh")) {
        return Err("usage: git-test-driver <local HOST|ssh CONFIG TARGET>".into());
    }
    let first_epoch = 5_000_000_000_001;
    let mut first = Connection::open(&arguments, first_epoch)?;
    let active = first.active_root()?;
    if !active.git_worktree {
        return Err("active root is not a Git worktree".into());
    }
    let initial = git_status(&mut first, &active)?;
    let repository = initial
        .repository
        .as_ref()
        .ok_or("status omitted repository")?;
    if repository.repository_id.is_empty()
        || !initial.authoritative
        || !initial
            .entries
            .iter()
            .any(|entry| entry.path == raw_path_fixture())
        || !initial.entries.iter().any(|entry| entry.ignored)
        || !initial
            .entries
            .iter()
            .any(|entry| entry.path == b"tracked" && entry.worktree_status == "M")
        || !initial
            .entries
            .iter()
            .any(|entry| entry.path == b":(glob)*")
    {
        return Err("initial remote Git status omitted raw/ignored/modified state".into());
    }

    let mut mutation = git_request(&first, &active, repository.repository_id.clone());
    mutation.path = b"tracked".to_vec();
    mutation.expected_status_generation = initial.generation;
    mutation.mutation = v1::GitMutationKind::StageFile.into();
    let staged = git_mutation(&mut first, mutation.clone())?;
    if staged.exit_code != 0
        || !staged.status.as_ref().is_some_and(|status| {
            status
                .entries
                .iter()
                .any(|entry| entry.path == b"tracked" && entry.index_status == "M")
        })
    {
        return Err("whole-file stage did not match Git CLI state".into());
    }

    let mut diff = git_request(&first, &active, repository.repository_id.clone());
    diff.path = b"tracked".to_vec();
    diff.diff_target = v1::GitDiffTarget::Staged.into();
    diff.expected_status_generation = staged
        .status
        .as_ref()
        .ok_or("stage omitted status")?
        .generation;
    let staged_diff = first
        .request(v1::Request {
            operation: v1::Operation::GitDiff.into(),
            git: Some(diff),
            ..Default::default()
        })?
        .git
        .and_then(|git| git.diff)
        .ok_or("staged diff omitted payload")?;
    if staged_diff.patch.is_empty() || staged_diff.source_generation.is_empty() {
        return Err("staged diff omitted patch/generation".into());
    }

    let staged_status = staged.status.unwrap();
    let mut commit = git_request(&first, &active, repository.repository_id.clone());
    commit.commit_message = "hook must block".into();
    commit.expected_status_generation = staged_status.generation;
    let commit = first
        .request(v1::Request {
            operation: v1::Operation::GitCommit.into(),
            git: Some(commit),
            ..Default::default()
        })?
        .git
        .and_then(|git| git.command)
        .ok_or("commit omitted command result")?;
    if commit.exit_code == 0
        || !commit
            .stderr
            .windows(b"phase5-hook-blocked".len())
            .any(|value| value == b"phase5-hook-blocked")
    {
        return Err("commit hook stderr/exit was not surfaced".into());
    }

    mutation.expected_status_generation = staged_status.generation;
    mutation.mutation = v1::GitMutationKind::UnstageFile.into();
    let unstaged = git_mutation(&mut first, mutation.clone())?;
    let unstaged_status = unstaged.status.ok_or("unstage omitted status")?;
    mutation.expected_status_generation = unstaged_status.generation;
    mutation.diff_target = v1::GitDiffTarget::Unstaged.into();
    mutation.mutation = v1::GitMutationKind::DiscardFile.into();
    let missing_confirmation = first.request_error(v1::Request {
        operation: v1::Operation::GitMutation.into(),
        git: Some(mutation.clone()),
        ..Default::default()
    })?;
    if missing_confirmation.error_code != "git_confirmation_required" {
        return Err("discard without token was not rejected host-side".into());
    }
    let confirmation = first
        .request(v1::Request {
            operation: v1::Operation::PrepareGitDiscard.into(),
            git: Some(mutation.clone()),
            ..Default::default()
        })?
        .git
        .and_then(|git| git.confirmation)
        .ok_or("prepare discard omitted token")?;
    mutation.confirmation_token = confirmation.token;
    let discarded = git_mutation(&mut first, mutation.clone())?;
    if discarded.exit_code != 0 {
        return Err("confirmed discard failed".into());
    }

    let mut escape = git_request(&first, &active, repository.repository_id.clone());
    escape.path = b"../escape".to_vec();
    escape.diff_target = v1::GitDiffTarget::Unstaged.into();
    escape.expected_status_generation = discarded
        .status
        .as_ref()
        .ok_or("discard omitted status")?
        .generation;
    let rejected = first.request_error(v1::Request {
        operation: v1::Operation::GitDiff.into(),
        git: Some(escape),
        ..Default::default()
    })?;
    if rejected.error_code != "git_rejected" {
        return Err("path escape did not fail closed".into());
    }

    // Leave a staged entry, then sever the transport while a deliberately
    // blocking hook is active. The host must cancel the Git process group and
    // the authoritative reconnect must still show the staged entry.
    let discarded_status = discarded.status.as_ref().ok_or("discard omitted status")?;
    let mut raw_stage = git_request(&first, &active, repository.repository_id.clone());
    raw_stage.path = raw_path_fixture();
    raw_stage.expected_status_generation = discarded_status.generation;
    raw_stage.mutation = v1::GitMutationKind::StageFile.into();
    let raw_staged = git_mutation(&mut first, raw_stage)?;
    let raw_status = raw_staged
        .status
        .as_ref()
        .ok_or("raw stage omitted status")?;
    install_blocking_hook(&arguments, &active.root)?;
    let mut interrupted_commit = git_request(&first, &active, repository.repository_id.clone());
    interrupted_commit.expected_status_generation = raw_status.generation;
    interrupted_commit.commit_message = "must be cancelled on transport loss".into();
    first.send_without_response(v1::Request {
        operation: v1::Operation::GitCommit.into(),
        git: Some(interrupted_commit),
        ..Default::default()
    })?;
    std::thread::sleep(std::time::Duration::from_millis(700));

    let before_reconnect_generation = raw_staged
        .status
        .as_ref()
        .map(|status| status.generation)
        .unwrap_or_default();
    drop(first);
    let second_epoch = first_epoch + 1;
    let mut second = Connection::open(&arguments, second_epoch)?;
    let recovered_root = second.active_root()?;
    let recovered = git_status(&mut second, &recovered_root)?;
    if recovered
        .repository
        .as_ref()
        .map(|repo| &repo.repository_id)
        != Some(&repository.repository_id)
        || !recovered.authoritative
        || !recovered
            .entries
            .iter()
            .any(|entry| entry.path == raw_path_fixture() && entry.index_status == "A")
    {
        return Err("reconnect did not replace Git state authoritatively".into());
    }
    let mut stale_epoch = git_request(&second, &recovered_root, repository.repository_id.clone());
    stale_epoch.connection_epoch = first_epoch;
    stale_epoch.path = raw_path_fixture();
    stale_epoch.expected_status_generation = recovered.generation;
    stale_epoch.mutation = v1::GitMutationKind::StageFile.into();
    let stale = second.request_error(v1::Request {
        operation: v1::Operation::GitMutation.into(),
        git: Some(stale_epoch),
        ..Default::default()
    })?;
    if stale.error_code != "stale_git_state" {
        return Err("pre-reconnect connection epoch was not rejected".into());
    }

    let mut literal = git_request(&second, &recovered_root, repository.repository_id.clone());
    literal.path = b":(glob)*".to_vec();
    literal.expected_status_generation = recovered.generation;
    literal.mutation = v1::GitMutationKind::StageFile.into();
    let literal = git_mutation(&mut second, literal)?;
    let literal_status = literal.status.ok_or("literal path stage omitted status")?;
    if !literal_status
        .entries
        .iter()
        .any(|entry| entry.path == b":(glob)*" && entry.index_status == "A")
        || !literal_status
            .entries
            .iter()
            .any(|entry| entry.path == b"ordinary" && entry.untracked)
    {
        return Err("literal magic pathspec staged a non-target path".into());
    }

    println!(
        "{}",
        serde_json::json!({
            "authoritativeStatus": true,
            "repositoryStableAcrossReconnect": true,
            "rawPathSafe": true,
            "ignored": true,
            "stage": true,
            "stagedDiff": true,
            "hookErrorSurfaced": true,
            "discardTokenEnforced": true,
            "pathEscapeRejected": true,
            "staleConnectionRejected": true,
            "literalPathspecSafe": true,
            "transportLossCancelledHook": true,
            "firstStatusGeneration": initial.generation.to_string(),
            "beforeReconnectGeneration": before_reconnect_generation.to_string(),
            "recoveredGeneration": recovered.generation.to_string(),
            "serverIdentity": second.hello.server_identity,
            "connectionEpoch": second.epoch.to_string(),
        })
    );
    Ok(())
}

fn install_blocking_hook(arguments: &[String], root: &str) -> Result<(), String> {
    let script = b"#!/bin/sh\ntrap '' TERM\nsleep 20\n";
    match arguments.get(1).map(String::as_str) {
        Some("local") => {
            let hook = std::path::Path::new(root).join(".git/hooks/pre-commit");
            fs::write(&hook, script).map_err(|error| error.to_string())?;
            let mut permissions = fs::metadata(&hook)
                .map_err(|error| error.to_string())?
                .permissions();
            permissions.set_mode(0o700);
            fs::set_permissions(hook, permissions).map_err(|error| error.to_string())
        }
        Some("ssh") => {
            let status = Command::new("ssh")
                .arg("-F")
                .arg(arguments.get(2).ok_or("SSH config required")?)
                .arg(arguments.get(3).ok_or("SSH target required")?)
                .arg("printf '%s\\n' '#!/bin/sh' \"trap '' TERM\" 'sleep 20' >\"$HOME/phase5-repo/.git/hooks/pre-commit\" && chmod 700 \"$HOME/phase5-repo/.git/hooks/pre-commit\"")
                .status()
                .map_err(|error| error.to_string())?;
            if status.success() {
                Ok(())
            } else {
                Err("failed to install remote blocking hook".into())
            }
        }
        _ => Err("unsupported driver mode".into()),
    }
}

fn git_status(
    connection: &mut Connection,
    active: &v1::ActiveRoot,
) -> Result<v1::GitStatusSnapshot, String> {
    connection
        .request(v1::Request {
            operation: v1::Operation::GitStatus.into(),
            git: Some(v1::GitRequest {
                operation_id: Uuid::new_v4().to_string(),
                root: active.root.clone(),
                root_token: active.root_token.clone(),
                expected_server_identity: active.server_identity.clone(),
                connection_epoch: connection.epoch,
                ..Default::default()
            }),
            ..Default::default()
        })?
        .git
        .and_then(|git| git.status)
        .ok_or("Git status omitted payload".into())
}

fn git_request(
    connection: &Connection,
    active: &v1::ActiveRoot,
    repository_id: String,
) -> v1::GitRequest {
    v1::GitRequest {
        operation_id: Uuid::new_v4().to_string(),
        root: active.root.clone(),
        root_token: active.root_token.clone(),
        expected_server_identity: active.server_identity.clone(),
        repository_id,
        connection_epoch: connection.epoch,
        ..Default::default()
    }
}

fn git_mutation(
    connection: &mut Connection,
    request: v1::GitRequest,
) -> Result<v1::GitCommandResult, String> {
    connection
        .request(v1::Request {
            operation: v1::Operation::GitMutation.into(),
            git: Some(request),
            ..Default::default()
        })?
        .git
        .and_then(|git| git.command)
        .ok_or("Git mutation omitted command result".into())
}

fn bridge_command(arguments: &[String]) -> Result<Command, String> {
    match arguments.get(1).map(String::as_str) {
        Some("local") => Ok(local_bridge_command(
            arguments.get(2).ok_or("host binary required")?,
        )),
        Some("ssh") => Ok(ssh_bridge_command(
            arguments.get(2).ok_or("SSH config required")?,
            arguments.get(3).ok_or("SSH target required")?,
            "$HOME/.local/bin/muxflow-host bridge --stdio",
            &[],
        )),
        _ => Err("unsupported driver mode".into()),
    }
}
