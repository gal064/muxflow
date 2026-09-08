use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};
use serde_json::{Map, Value};
use tmux_agent_protocol::v1;

use super::adapters;

const MAX_CONFIG_BYTES: u64 = 2 * 1024 * 1024;
const MAX_PREVIEW_BYTES: usize = 32 * 1024;

/// What one adapter's configuration currently does with lifecycle events.
///
/// This is an observation, not a capability: an adapter that supports hooks on
/// every host is still `NotWired` on a host whose configuration routes its
/// events somewhere else, which is exactly the field failure this reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AdapterWiring {
    pub adapter_id: &'static str,
    pub config_path: PathBuf,
    pub state: v1::AgentHookWiring,
    /// Only populated for `Unavailable`; never a summary of a healthy state.
    pub detail: String,
}

#[derive(Debug, Clone)]
pub(crate) struct HookManager {
    home: PathBuf,
    helper_path: PathBuf,
    /// Replaces one named adapter's configuration path, and only that
    /// adapter's. Set by the CLI's `--settings-path`, which is how QA exercises
    /// a real-shaped fixture without touching a real `~/.claude`.
    ///
    /// The adapter is part of the value rather than a rule enforced at the call
    /// site: an override that silently applied to every adapter had Codex's
    /// wiring read out of Claude Code's settings file.
    config_override: Option<(&'static str, PathBuf)>,
}

impl HookManager {
    #[cfg(test)]
    pub(super) fn for_home(home: &Path) -> Self {
        Self {
            home: home.into(),
            helper_path: PathBuf::from("/opt/muxflow/bin/muxflow-host"),
            config_override: None,
        }
    }

    pub(crate) fn system_default() -> anyhow::Result<Self> {
        Self::with_overrides(None, None)
    }

    /// The CLI's entry point. `home` relocates every adapter's configuration;
    /// `config_override` relocates exactly the adapter it names.
    pub(crate) fn with_overrides(
        home: Option<PathBuf>,
        config_override: Option<(&'static str, PathBuf)>,
    ) -> anyhow::Result<Self> {
        let home = match home {
            Some(home) => home,
            None => PathBuf::from(std::env::var_os("HOME").context("HOME is unavailable")?),
        };
        if !home.is_absolute() {
            bail!("hook home must be an absolute path");
        }
        if let Some((_, path)) = config_override.as_ref()
            && !path.is_absolute()
        {
            bail!("hook settings path must be absolute");
        }
        let helper_path = std::env::current_exe()
            .context("resolve current hook helper executable")?
            .canonicalize()
            .context("canonicalize current hook helper executable")?;
        if !helper_path.is_absolute() {
            bail!("hook helper path must be absolute");
        }
        Ok(Self {
            home,
            helper_path,
            config_override,
        })
    }

    fn config_path(&self, adapter: &dyn adapters::AgentAdapter) -> PathBuf {
        self.config_override
            .as_ref()
            .filter(|(id, _)| *id == adapter.id())
            .map(|(_, path)| path.clone())
            .unwrap_or_else(|| adapter.hook_path(&self.home))
    }

    /// Each adapter paired with its own answer to "can a lifecycle event from
    /// this agent ever reach this daemon on this host".
    ///
    /// Failure to inspect is reported as `Unavailable`, never as `NotWired`:
    /// proposing an install over a configuration nobody could read is how
    /// unrelated hooks get lost.
    ///
    /// Pairs rather than two parallel lists: the consumer that builds the
    /// snapshot's descriptors used to walk the registry a second time and zip
    /// the results, which made "these two functions iterate the same registry
    /// in the same order" a rule enforced by a comment and a debug assertion,
    /// with silent truncation if it were ever broken.
    pub(crate) fn wiring(&self) -> Vec<ObservedAdapter> {
        self.wiring_with_running(&Default::default())
    }

    /// `running` names adapters with an agent live on this host right now.
    ///
    /// A running agent is proof its vendor is installed, whatever
    /// [`Self::agent_is_present`] concluded — and that probe reads the *daemon
    /// process's* `PATH`, which under launchd or a non-login SSH exec has no
    /// `~/.local/bin` in it. Taken here so one function decides one answer;
    /// the correction used to be applied afterwards, by the snapshot builder.
    pub(crate) fn wiring_with_running(
        &self,
        running: &std::collections::BTreeSet<&str>,
    ) -> Vec<ObservedAdapter> {
        adapters::all()
            .map(|adapter| {
                let config_path = self.config_path(adapter);
                let observed = match self.inspect_wiring(adapter, &config_path, running) {
                    Ok(state) => AdapterWiring {
                        adapter_id: adapter.id(),
                        config_path,
                        state,
                        detail: String::new(),
                    },
                    Err(error) => AdapterWiring {
                        adapter_id: adapter.id(),
                        config_path,
                        state: v1::AgentHookWiring::Unavailable,
                        detail: error.to_string(),
                    },
                };
                (adapter, observed)
            })
            .collect()
    }

    fn inspect_wiring(
        &self,
        adapter: &'static dyn adapters::AgentAdapter,
        path: &Path,
        running: &std::collections::BTreeSet<&str>,
    ) -> anyhow::Result<v1::AgentHookWiring> {
        inspect_config_path(path)?;
        let bytes = read_config(path)?;
        let value = parse_config(&bytes)?;
        validate_hook_shape(&value, adapter.hook_events())?;
        Ok(
            if managed_entries_are_current(&value, adapter, &self.helper_path) {
                v1::AgentHookWiring::Wired
            } else if managed_entry_count(&value, adapter) > 0 {
                v1::AgentHookWiring::Partial
            } else if bytes.is_empty()
                && !running.contains(adapter.id())
                && !self.agent_is_present(adapter)
            {
                v1::AgentHookWiring::Absent
            } else {
                v1::AgentHookWiring::NotWired
            },
        )
    }

    /// Whether this agent exists on the host at all.
    ///
    /// Without this the desktop offers to "set up" an agent nobody has
    /// installed, and accepting creates a configuration directory and file for
    /// a vendor the user does not use — because an absent config reads exactly
    /// like an unwired one. Any of three things counts as present: a
    /// configuration file, the directory that would hold it, or the
    /// executable on PATH.
    fn agent_is_present(&self, adapter: &dyn adapters::AgentAdapter) -> bool {
        self.agent_is_present_in(adapter, std::env::var_os("PATH"))
    }

    /// The search path is a parameter so a test can ask the question without
    /// the answer depending on what happens to be installed on the machine
    /// running it.
    fn agent_is_present_in(
        &self,
        adapter: &dyn adapters::AgentAdapter,
        search_path: Option<std::ffi::OsString>,
    ) -> bool {
        let config = self.config_path(adapter);
        if config.exists() || config.parent().is_some_and(Path::exists) {
            return true;
        }
        let executable = adapter.executable();
        search_path
            .map(|path| std::env::split_paths(&path).any(|dir| dir.join(executable).exists()))
            .unwrap_or(false)
    }

    /// Return the adapters whose configuration still contains a hook owned by
    /// this application. This intentionally uses the same parser and ownership
    /// rules as install/uninstall so packaging cannot drift from hook formats.
    pub(crate) fn managed_adapters(&self) -> anyhow::Result<Vec<&'static str>> {
        let mut managed = Vec::new();
        for kind in [
            v1::AgentAdapterKind::Codex,
            v1::AgentAdapterKind::ClaudeCode,
        ] {
            let adapter = adapters::adapter(kind).context("agent adapter is required")?;
            let path = self.config_path(adapter);
            inspect_config_path(&path)?;
            let value = parse_config(&read_config(&path)?)?;
            validate_hook_shape(&value, adapter.hook_events())?;
            if managed_entry_count(&value, adapter) > 0 {
                managed.push(adapter.id());
            }
        }
        Ok(managed)
    }

    pub(crate) fn review(
        &self,
        adapter: v1::AgentAdapterKind,
        action: v1::HookManagementAction,
    ) -> anyhow::Result<v1::HookManagementPlan> {
        let adapter_impl = adapters::adapter(adapter).context("agent adapter is required")?;
        let path = self.config_path(adapter_impl);
        inspect_config_path(&path)?;
        let bytes = read_config(&path)?;
        let value = parse_config(&bytes)?;
        ensure_no_future_managed(&value, adapter_impl)?;
        validate_hook_shape(&value, adapter_impl.hook_events())?;
        let already_current = match action {
            v1::HookManagementAction::Install => {
                managed_entries_are_current(&value, adapter_impl, &self.helper_path)
            }
            v1::HookManagementAction::Uninstall => managed_entry_count(&value, adapter_impl) == 0,
            _ => false,
        };
        let confirmation_token = confirmation_token(adapter, action, &bytes);
        let mut proposed = value.clone();
        remove_managed(&mut proposed, adapter_impl);
        if action == v1::HookManagementAction::Install {
            add_managed(&mut proposed, adapter_impl, &self.helper_path);
        }
        let removes_config = action == v1::HookManagementAction::Uninstall
            && proposed.as_object().is_some_and(Map::is_empty)
            && !backup_path(&path).exists();
        let proposed_bytes = if removes_config {
            Vec::new()
        } else {
            serde_json::to_vec_pretty(&proposed)?
        };
        // Each once. Every one of these deep-clones a configuration of up to
        // two megabytes, redacts it and pretty-prints it, and `apply` calls
        // `review` three times.
        let before_preview = preview(&value);
        let after_preview = preview(&proposed);
        let diff_preview = diff_preview(&value, &proposed);
        Ok(v1::HookManagementPlan {
            adapter: adapter.into(),
            adapter_id: adapter_impl.id().into(),
            action: action.into(),
            config_path: path.to_string_lossy().into_owned(),
            backup_path: backup_path(&path).to_string_lossy().into_owned(),
            managed_version: adapters::MANAGED_VERSION.to_string(),
            summary: match action {
                v1::HookManagementAction::Install => {
                    "Merge labeled lifecycle hooks; preserve every unrelated hook and setting"
                }
                v1::HookManagementAction::Uninstall => {
                    "Remove only muxflow labeled hooks; preserve backup and unrelated config"
                }
                _ => "Review managed hook configuration",
            }
            .into(),
            confirmation_token,
            already_current,
            proposed_events: adapter_impl
                .hook_events()
                .iter()
                .map(|event| (*event).into())
                .collect(),
            proposed_command: adapter_impl.hook_command(&self.helper_path),
            ownership_marker: format!(
                "owner={};version={}",
                adapters::MANAGED_OWNER,
                adapters::MANAGED_VERSION
            ),
            trust_guidance: adapter_impl.hook_trust_guidance().into(),
            before_hash: blake3::hash(&bytes).to_hex().to_string(),
            after_hash: blake3::hash(&proposed_bytes).to_hex().to_string(),
            creates_config: bytes.is_empty() && action == v1::HookManagementAction::Install,
            removes_config,
            before_preview: before_preview.0,
            after_preview: after_preview.0,
            diff_preview: diff_preview.0,
            preview_truncated: before_preview.1 || after_preview.1 || diff_preview.1,
        })
    }

    pub(crate) fn apply(
        &self,
        adapter: v1::AgentAdapterKind,
        action: v1::HookManagementAction,
        confirmation_token_value: &str,
    ) -> anyhow::Result<v1::HookManagementPlan> {
        if !matches!(
            action,
            v1::HookManagementAction::Install | v1::HookManagementAction::Uninstall
        ) {
            bail!("hook action must be install or uninstall");
        }
        let review = self.review(adapter, action)?;
        if confirmation_token_value != review.confirmation_token {
            bail!("hook review is stale; review the current configuration again");
        }
        if review.already_current {
            return Ok(review);
        }
        let adapter_impl = adapters::adapter(adapter).context("agent adapter is required")?;
        let path = self.config_path(adapter_impl);
        let _lock = ConfigLock::acquire(&path)?;
        let review = self.review(adapter, action)?;
        if confirmation_token_value != review.confirmation_token {
            bail!("hook configuration changed while acquiring its exclusive lock");
        }
        let bytes = read_config(&path)?;
        if confirmation_token(adapter, action, &bytes) != review.confirmation_token {
            bail!("hook review changed while acquiring the configuration lock");
        }
        let mut value = parse_config(&bytes)?;
        remove_managed(&mut value, adapter_impl);
        if action == v1::HookManagementAction::Install {
            add_managed(&mut value, adapter_impl, &self.helper_path);
            write_backup_once(&path, &bytes)?;
            write_atomic(&path, &serde_json::to_vec_pretty(&value)?, &bytes)?;
        } else if value.as_object().is_some_and(Map::is_empty) && !backup_path(&path).exists() {
            remove_atomic(&path, &bytes)?;
        } else {
            write_atomic(&path, &serde_json::to_vec_pretty(&value)?, &bytes)?;
        }
        self.review(adapter, action)
    }
}

/// The wiring an agent snapshot carries, re-read only when a configuration file
/// actually changed.
///
/// Snapshots are requested on every topology generation, so parsing two
/// configuration files each time would put file I/O on a path the phase budgets
/// at under a second end to end. The fingerprint is the identity a change would
/// have to alter — path, size, modification time, and the helper the command
/// would name — so an install performed a millisecond ago is still observed
/// immediately, unlike a time-based cache.
///
/// Owned by whoever builds snapshots rather than by a file-scoped static, so it
/// is per-runtime and reachable from a test.
///
/// One adapter and its own observation, as [`HookManager::wiring`] pairs them.
pub(crate) type ObservedAdapter = (&'static dyn adapters::AgentAdapter, AdapterWiring);

#[derive(Default)]
pub(crate) struct WiringCache {
    /// Built once. `HookManager::system_default` resolves and canonicalizes
    /// this process's own executable, and doing that per snapshot puts
    /// syscalls back on the path the cache exists to keep clear. Neither the
    /// home directory nor the running executable changes under a live daemon.
    manager: Option<HookManager>,
    observed: Option<(Vec<u8>, Vec<ObservedAdapter>)>,
}

impl WiringCache {
    pub(crate) fn current(
        &mut self,
        running: &std::collections::BTreeSet<&str>,
    ) -> Vec<ObservedAdapter> {
        let manager = match self.manager.take() {
            Some(manager) => manager,
            None => match HookManager::system_default() {
                Ok(manager) => manager,
                // An environment without a resolvable home or executable, not
                // a configuration file that failed to parse. The desktop
                // renders this reason verbatim, so it carries the real error
                // rather than a fixed sentence guessing which of the several
                // ways `system_default` can fail actually happened.
                Err(error) => {
                    let detail = error.to_string();
                    return adapters::all()
                        .map(|adapter| {
                            (
                                adapter,
                                AdapterWiring {
                                    adapter_id: adapter.id(),
                                    config_path: PathBuf::new(),
                                    state: v1::AgentHookWiring::Unavailable,
                                    detail: detail.clone(),
                                },
                            )
                        })
                        .collect();
                }
            },
        };
        let wiring = Self::memoized(&mut self.observed, &manager, running);
        self.manager = Some(manager);
        wiring
    }

    /// Static, over the memo alone: as a method it needed `&mut self` while
    /// `self.manager` was borrowed, which forced `current` to take its own
    /// field out and put it back — and to lose it on any panic in between.
    fn memoized(
        observed: &mut Option<(Vec<u8>, Vec<ObservedAdapter>)>,
        manager: &HookManager,
        running: &std::collections::BTreeSet<&str>,
    ) -> Vec<ObservedAdapter> {
        // The running set is part of the fingerprint: an agent starting is a
        // reason to re-read, and it is exactly the case the `PATH` probe gets
        // wrong.
        let mut fingerprint = manager.wiring_fingerprint();
        for adapter in running {
            fingerprint.push(0);
            fingerprint.extend_from_slice(adapter.as_bytes());
        }
        if let Some((taken, wiring)) = observed.as_ref()
            && taken == &fingerprint
        {
            return wiring.clone();
        }
        let wiring = manager.wiring_with_running(running);
        *observed = Some((fingerprint, wiring.clone()));
        wiring
    }
}

impl HookManager {
    /// `symlink_metadata`, deliberately: a symlinked configuration is refused
    /// by `read_config`'s `O_NOFOLLOW` and always reports `Unavailable`, so the
    /// link's own identity is both cheaper and the right thing to key on.
    fn wiring_fingerprint(&self) -> Vec<u8> {
        let mut fingerprint = Vec::new();
        fingerprint.extend_from_slice(self.helper_path.to_string_lossy().as_bytes());
        for adapter in adapters::all() {
            let path = self.config_path(adapter);
            fingerprint.push(0);
            fingerprint.extend_from_slice(path.to_string_lossy().as_bytes());
            let metadata = fs::symlink_metadata(&path).ok();
            fingerprint.extend_from_slice(
                &metadata
                    .as_ref()
                    .map(|metadata| metadata.len())
                    .unwrap_or_default()
                    .to_le_bytes(),
            );
            let modified = metadata
                .as_ref()
                .and_then(|metadata| metadata.modified().ok())
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_nanos())
                .unwrap_or_default();
            fingerprint.extend_from_slice(&modified.to_le_bytes());
        }
        fingerprint
    }
}

fn read_config(path: &Path) -> anyhow::Result<Vec<u8>> {
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_CONFIG_BYTES {
        bail!("hook configuration must be a bounded regular file");
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

#[cfg(not(target_os = "linux"))]
fn ensure_unchanged(path: &Path, reviewed: &[u8]) -> anyhow::Result<()> {
    if read_config(path)? != reviewed {
        bail!("hook configuration changed after review; review again");
    }
    Ok(())
}

struct ConfigLock {
    path: PathBuf,
    file: fs::File,
}

impl ConfigLock {
    fn acquire(config: &Path) -> anyhow::Result<Self> {
        let parent = config.parent().context("hook config path has no parent")?;
        if !parent.exists() {
            fs::create_dir_all(parent)?;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        }
        inspect_parent(parent)?;
        let path = parent.join(".muxflow-hook.lock");
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
            .context("another hook configuration update is in progress")?;
        // Advisory flock has process-lifetime cleanup, unlike a create-new
        // sentinel which can permanently wedge after a crash.
        // SAFETY: flock only receives the live lock-file descriptor and flags.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            bail!("another hook configuration update is in progress");
        }
        Ok(Self { path, file })
    }
}

impl Drop for ConfigLock {
    fn drop(&mut self) {
        // SAFETY: the descriptor remains owned by self until after this call.
        unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
        let _ = fs::remove_file(&self.path);
    }
}

fn parse_config(bytes: &[u8]) -> anyhow::Result<Value> {
    if bytes.is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    let value: Value = serde_json::from_slice(bytes).context("parse hook JSON configuration")?;
    if !value.is_object() {
        bail!("hook configuration root must be a JSON object");
    }
    Ok(value)
}

fn validate_hook_shape(value: &Value, events: &[&str]) -> anyhow::Result<()> {
    let Some(hooks) = value.get("hooks") else {
        return Ok(());
    };
    let hooks = hooks
        .as_object()
        .context("existing hooks setting must be a JSON object")?;
    for event in events {
        if let Some(groups) = hooks.get(*event)
            && !groups.is_array()
        {
            bail!("existing {event} hooks must be a JSON array");
        }
    }
    Ok(())
}

fn add_managed(value: &mut Value, adapter: &dyn adapters::AgentAdapter, helper_path: &Path) {
    let root = value.as_object_mut().expect("validated object");
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    let hooks = hooks
        .as_object_mut()
        .expect("review validated hooks object");
    let command = adapter.hook_command(helper_path);
    for event in adapter.hook_events() {
        let groups = hooks
            .entry((*event).to_owned())
            .or_insert_with(|| Value::Array(Vec::new()));
        groups
            .as_array_mut()
            .expect("review validated event array")
            .push(serde_json::json!({
                "hooks": [{"type": "command", "command": command, "timeout": 5}]
            }));
    }
}

fn remove_managed(value: &mut Value, adapter: &dyn adapters::AgentAdapter) {
    let Some(hooks) = value.get_mut("hooks").and_then(Value::as_object_mut) else {
        return;
    };
    for groups in hooks.values_mut().filter_map(Value::as_array_mut) {
        for group in groups.iter_mut() {
            let Some(commands) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
                continue;
            };
            commands.retain(|command| {
                !command
                    .get("command")
                    .and_then(Value::as_str)
                    .is_some_and(|command| is_owned_command(command, adapter))
            });
        }
        groups.retain(|group| {
            group
                .get("hooks")
                .and_then(Value::as_array)
                .is_none_or(|commands| !commands.is_empty())
        });
    }
    hooks.retain(|_, groups| !groups.as_array().is_some_and(Vec::is_empty));
    if hooks.is_empty() {
        value.as_object_mut().unwrap().remove("hooks");
    }
}

fn managed_entry_count(value: &Value, adapter: &dyn adapters::AgentAdapter) -> usize {
    value
        .get("hooks")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|hooks| hooks.values())
        .filter_map(Value::as_array)
        .flatten()
        .flat_map(|group| {
            group
                .get("hooks")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter(|command| {
            command
                .get("command")
                .and_then(Value::as_str)
                .is_some_and(|command| is_owned_command(command, adapter))
        })
        .count()
}

fn managed_entries_are_current(
    value: &Value,
    adapter: &dyn adapters::AgentAdapter,
    helper_path: &Path,
) -> bool {
    let Some(hooks) = value.get("hooks").and_then(Value::as_object) else {
        return false;
    };
    let expected = adapter.hook_command(helper_path);
    managed_entry_count(value, adapter) == adapter.hook_events().len()
        && adapter.hook_events().iter().all(|event| {
            hooks
                .get(*event)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .flat_map(|group| {
                    group
                        .get("hooks")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                })
                .any(|command| {
                    command
                        .get("command")
                        .and_then(Value::as_str)
                        .is_some_and(|command| command == expected)
                })
        })
}

fn is_owned_command(command: &str, adapter: &dyn adapters::AgentAdapter) -> bool {
    managed_command_version(command, adapter)
        .is_some_and(|version| version <= adapters::MANAGED_VERSION)
}

fn managed_command_version(command: &str, adapter: &dyn adapters::AgentAdapter) -> Option<u32> {
    let legacy = format!(
        "muxflow-host hook ingest --adapter {} # muxflow-managed:v1",
        adapter.id()
    );
    if command == legacy {
        return Some(1);
    }
    let (executable, suffix) = command.split_once(" hook ingest --adapter ")?;
    if !executable.starts_with("'/") || !executable.ends_with('\'') || executable.contains('\n') {
        return None;
    }
    let version = suffix.strip_prefix(&format!(
        "{} --managed-owner {} --managed-version ",
        adapter.id(),
        adapters::MANAGED_OWNER
    ))?;
    version.parse().ok()
}

fn ensure_no_future_managed(
    value: &Value,
    adapter: &dyn adapters::AgentAdapter,
) -> anyhow::Result<()> {
    let future = value
        .get("hooks")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|hooks| hooks.values())
        .filter_map(Value::as_array)
        .flatten()
        .flat_map(|group| {
            group
                .get("hooks")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter_map(|command| command.get("command").and_then(Value::as_str))
        .filter_map(|command| managed_command_version(command, adapter))
        .any(|version| version > adapters::MANAGED_VERSION);
    if future {
        bail!("hook configuration is owned by a newer muxflow version");
    }
    Ok(())
}

fn preview(value: &Value) -> (String, bool) {
    let mut redacted = value.clone();
    redact(&mut redacted);
    bounded(serde_json::to_string_pretty(&redacted).unwrap_or_else(|_| "{}".into()))
}

fn diff_preview(before: &Value, after: &Value) -> (String, bool) {
    let before = preview(before).0;
    let after = preview(after).0;
    bounded(format!("--- before\n{before}\n+++ after\n{after}"))
}

fn bounded(mut value: String) -> (String, bool) {
    if value.len() <= MAX_PREVIEW_BYTES {
        return (value, false);
    }
    const SUFFIX: &str = "\n… preview truncated …";
    let mut boundary = MAX_PREVIEW_BYTES - SUFFIX.len();
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
    value.push_str(SUFFIX);
    (value, true)
}

fn redact(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                let key = key.to_ascii_lowercase();
                if [
                    "token",
                    "secret",
                    "password",
                    "credential",
                    "private",
                    "authorization",
                    "api_key",
                    "apikey",
                ]
                .iter()
                .any(|sensitive| key.contains(sensitive))
                {
                    *value = Value::String("<redacted>".into());
                } else {
                    redact(value);
                }
            }
        }
        Value::Array(values) => values.iter_mut().for_each(redact),
        _ => {}
    }
}

fn inspect_config_path(path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent().filter(|parent| parent.exists()) {
        inspect_parent(parent)?;
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("refusing to mutate a non-regular or symlinked hook configuration");
    }
    if metadata.len() > MAX_CONFIG_BYTES {
        bail!("hook configuration exceeds the {MAX_CONFIG_BYTES}-byte safety limit");
    }
    let backup = backup_path(path);
    if let Ok(metadata) = fs::symlink_metadata(&backup)
        && (metadata.file_type().is_symlink() || !metadata.is_file())
    {
        bail!("managed hook backup path is not a regular file");
    }
    Ok(())
}

fn inspect_parent(parent: &Path) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(parent)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("refusing to mutate hook configuration through an unsafe parent directory");
    }
    Ok(())
}

fn confirmation_token(
    adapter: v1::AgentAdapterKind,
    action: v1::HookManagementAction,
    bytes: &[u8],
) -> String {
    let mut hash = blake3::Hasher::new();
    hash.update(&(adapter as i32).to_le_bytes());
    hash.update(&(action as i32).to_le_bytes());
    hash.update(&adapters::MANAGED_VERSION.to_le_bytes());
    hash.update(bytes);
    hash.finalize().to_hex().to_string()
}

fn backup_path(path: &Path) -> PathBuf {
    path.with_extension(format!(
        "{}.muxflow.backup",
        path.extension()
            .and_then(|value| value.to_str())
            .unwrap_or("json")
    ))
}

fn write_backup_once(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if bytes.is_empty() || backup_path(path).exists() {
        return Ok(());
    }
    write_new_private(&backup_path(path), bytes)
}

fn write_atomic(path: &Path, bytes: &[u8], reviewed: &[u8]) -> anyhow::Result<()> {
    let parent = path.parent().context("hook config path has no parent")?;
    if !parent.exists() {
        fs::create_dir_all(parent)?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    inspect_parent(parent)?;
    let temporary = parent.join(format!(".hook-config-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> anyhow::Result<()> {
        write_new_private(&temporary, bytes)?;
        // This is intentionally the last fallible observation before replace:
        // an editor that raced the reviewed bytes is never silently clobbered.
        inspect_parent(parent)?;
        #[cfg(target_os = "linux")]
        if reviewed.is_empty() {
            renameat2(&temporary, path, libc::RENAME_NOREPLACE)?;
        } else {
            exchange_reviewed(&temporary, path, reviewed)?;
        }
        #[cfg(not(target_os = "linux"))]
        {
            ensure_unchanged(path, reviewed)?;
            fs::rename(&temporary, path)?;
        }
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

#[cfg(target_os = "linux")]
fn exchange_reviewed(temporary: &Path, path: &Path, reviewed: &[u8]) -> anyhow::Result<()> {
    renameat2(temporary, path, libc::RENAME_EXCHANGE)?;
    if read_config(temporary)? != reviewed {
        renameat2(temporary, path, libc::RENAME_EXCHANGE)
            .context("roll back raced hook configuration")?;
        bail!("hook configuration changed during atomic replace; review again");
    }
    fs::remove_file(temporary)?;
    Ok(())
}

fn remove_atomic(path: &Path, reviewed: &[u8]) -> anyhow::Result<()> {
    let parent = path.parent().context("hook config path has no parent")?;
    inspect_parent(parent)?;
    #[cfg(target_os = "linux")]
    {
        let tombstone = parent.join(format!(".hook-remove-{}.tmp", uuid::Uuid::new_v4()));
        renameat2(path, &tombstone, libc::RENAME_NOREPLACE)?;
        if read_config(&tombstone)? != reviewed {
            renameat2(&tombstone, path, libc::RENAME_NOREPLACE)
                .context("roll back raced hook uninstall")?;
            bail!("hook configuration changed during atomic uninstall; review again");
        }
        fs::remove_file(tombstone)?;
    }
    #[cfg(not(target_os = "linux"))]
    {
        ensure_unchanged(path, reviewed)?;
        fs::remove_file(path)?;
    }
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn renameat2(from: &Path, to: &Path, flags: libc::c_uint) -> anyhow::Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let from = CString::new(from.as_os_str().as_bytes()).context("temporary path contains NUL")?;
    let to = CString::new(to.as_os_str().as_bytes()).context("hook path contains NUL")?;
    // SAFETY: both C strings are live for the syscall and AT_FDCWD scopes the
    // operation to the exact validated paths.
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            flags,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().into())
    }
}

fn write_new_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_upgrade_and_uninstall_preserve_unrelated_configuration() {
        let home = std::env::current_dir()
            .unwrap()
            .join("tmp")
            .join(format!("phase6-hooks-{}", uuid::Uuid::new_v4()));
        let path = home.join(".codex/hooks.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = br#"{"theme":"dark","hooks":{"Stop":[{"hooks":[{"type":"command","command":"keep-me"}]}]}}"#;
        fs::write(&path, original).unwrap();
        let manager = HookManager::for_home(&home);
        let review = manager
            .review(
                v1::AgentAdapterKind::Codex,
                v1::HookManagementAction::Install,
            )
            .unwrap();
        assert_eq!(
            review.proposed_events,
            adapter_events(v1::AgentAdapterKind::Codex)
        );
        assert_eq!(
            review.proposed_command,
            "'/opt/muxflow/bin/muxflow-host' hook ingest --adapter codex --managed-owner muxflow --managed-version 4"
        );
        assert_eq!(review.ownership_marker, "owner=muxflow;version=4");
        assert!(review.trust_guidance.contains("never edits or bypasses"));
        assert!(review.before_preview.contains("keep-me"));
        assert!(review.after_preview.contains("--managed-owner"));
        assert!(review.diff_preview.starts_with("--- before"));
        assert_ne!(review.before_hash, review.after_hash);
        manager
            .apply(
                v1::AgentAdapterKind::Codex,
                v1::HookManagementAction::Install,
                &review.confirmation_token,
            )
            .unwrap();
        let installed: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let adapter = adapters::adapter(v1::AgentAdapterKind::Codex).unwrap();
        assert_eq!(installed["theme"], "dark");
        assert_eq!(
            managed_entry_count(&installed, adapter),
            adapter.hook_events().len()
        );
        assert!(installed["hooks"].get("Notification").is_none());
        assert!(installed.to_string().contains("keep-me"));
        let repeated = manager
            .review(
                v1::AgentAdapterKind::Codex,
                v1::HookManagementAction::Install,
            )
            .unwrap();
        assert!(repeated.already_current);
        let uninstall = manager
            .review(
                v1::AgentAdapterKind::Codex,
                v1::HookManagementAction::Uninstall,
            )
            .unwrap();
        manager
            .apply(
                v1::AgentAdapterKind::Codex,
                v1::HookManagementAction::Uninstall,
                &uninstall.confirmation_token,
            )
            .unwrap();
        let removed: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(managed_entry_count(&removed, adapter), 0);
        assert!(removed.to_string().contains("keep-me"));
        assert_eq!(fs::read(backup_path(&path)).unwrap(), original);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn managed_status_detects_current_and_legacy_entries_across_adapters() {
        let home = std::env::current_dir()
            .unwrap()
            .join("tmp")
            .join(format!("phase8-hook-status-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(home.join(".codex")).unwrap();
        fs::create_dir_all(home.join(".claude")).unwrap();
        let current = adapters::adapter(v1::AgentAdapterKind::Codex)
            .unwrap()
            .hook_command(Path::new("/opt/muxflow/bin/muxflow-host"));
        let legacy = "muxflow-host hook ingest --adapter claude-code # muxflow-managed:v1";
        fs::write(
            home.join(".codex/hooks.json"),
            serde_json::to_vec(&serde_json::json!({
                "hooks": {"Stop": [{"hooks": [{"type": "command", "command": current}]}]}
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            home.join(".claude/settings.json"),
            serde_json::to_vec(&serde_json::json!({
                "hooks": {"Stop": [{"hooks": [{"type": "command", "command": legacy}]}]}
            }))
            .unwrap(),
        )
        .unwrap();
        let manager = HookManager::for_home(&home);
        assert_eq!(
            manager.managed_adapters().unwrap(),
            ["codex", "claude-code"]
        );
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn stale_review_token_cannot_mutate_configuration() {
        let home = std::env::current_dir()
            .unwrap()
            .join("tmp")
            .join(format!("phase6-hook-token-{}", uuid::Uuid::new_v4()));
        let manager = HookManager::for_home(&home);
        let error = manager
            .apply(
                v1::AgentAdapterKind::ClaudeCode,
                v1::HookManagementAction::Install,
                "stale",
            )
            .unwrap_err();
        assert!(error.to_string().contains("stale"));
        assert!(!home.exists());
    }

    #[test]
    fn install_then_uninstall_of_new_config_restores_absence() {
        let home = std::env::current_dir()
            .unwrap()
            .join("tmp")
            .join(format!("phase6-hook-new-{}", uuid::Uuid::new_v4()));
        let manager = HookManager::for_home(&home);
        let install = manager
            .review(
                v1::AgentAdapterKind::ClaudeCode,
                v1::HookManagementAction::Install,
            )
            .unwrap();
        manager
            .apply(
                v1::AgentAdapterKind::ClaudeCode,
                v1::HookManagementAction::Install,
                &install.confirmation_token,
            )
            .unwrap();
        let installed: Value =
            serde_json::from_slice(&fs::read(home.join(".claude/settings.json")).unwrap()).unwrap();
        let adapter = adapters::adapter(v1::AgentAdapterKind::ClaudeCode).unwrap();
        assert_eq!(
            managed_entry_count(&installed, adapter),
            adapter.hook_events().len()
        );
        assert!(installed["hooks"]["Notification"].is_array());
        let uninstall = manager
            .review(
                v1::AgentAdapterKind::ClaudeCode,
                v1::HookManagementAction::Uninstall,
            )
            .unwrap();
        manager
            .apply(
                v1::AgentAdapterKind::ClaudeCode,
                v1::HookManagementAction::Uninstall,
                &uninstall.confirmation_token,
            )
            .unwrap();
        assert!(!home.join(".claude/settings.json").exists());
        assert!(!backup_path(&home.join(".claude/settings.json")).exists());
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn symlinked_config_fails_closed() {
        use std::os::unix::fs::symlink;

        let home = std::env::current_dir()
            .unwrap()
            .join("tmp")
            .join(format!("phase6-hook-link-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(home.join(".codex")).unwrap();
        fs::write(home.join("target.json"), b"{}").unwrap();
        symlink(home.join("target.json"), home.join(".codex/hooks.json")).unwrap();
        let manager = HookManager::for_home(&home);
        assert!(
            manager
                .review(
                    v1::AgentAdapterKind::Codex,
                    v1::HookManagementAction::Install
                )
                .is_err()
        );
        assert_eq!(fs::read(home.join("target.json")).unwrap(), b"{}");
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn symlinked_config_parent_fails_closed() {
        use std::os::unix::fs::symlink;

        let home = std::env::current_dir()
            .unwrap()
            .join("tmp")
            .join(format!("phase6-hook-parent-link-{}", uuid::Uuid::new_v4()));
        let target = home.join("real-codex");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("hooks.json"), b"{}").unwrap();
        symlink(&target, home.join(".codex")).unwrap();
        let manager = HookManager::for_home(&home);
        assert!(
            manager
                .review(
                    v1::AgentAdapterKind::Codex,
                    v1::HookManagementAction::Install
                )
                .is_err()
        );
        assert_eq!(fs::read(target.join("hooks.json")).unwrap(), b"{}");
        fs::remove_dir_all(home).unwrap();
    }

    fn adapter_events(kind: v1::AgentAdapterKind) -> Vec<String> {
        adapters::adapter(kind)
            .unwrap()
            .hook_events()
            .iter()
            .map(|event| (*event).into())
            .collect()
    }

    #[test]
    fn unreadable_config_is_not_treated_as_absent() {
        let home = std::env::current_dir()
            .unwrap()
            .join("tmp")
            .join(format!("phase6-hook-unreadable-{}", uuid::Uuid::new_v4()));
        let path = home.join(".claude/settings.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"{}").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
        let manager = HookManager::for_home(&home);
        let result = manager.review(
            v1::AgentAdapterKind::ClaudeCode,
            v1::HookManagementAction::Install,
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(result.is_err());
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn uninstall_matches_the_exact_owned_command_only() {
        let adapter = adapters::adapter(v1::AgentAdapterKind::Codex).unwrap();
        let helper = Path::new("/opt/muxflow/bin/muxflow-host");
        let owned = adapter.hook_command(helper);
        let lookalike = format!("{owned} --extra");
        let mut value = serde_json::json!({"hooks":{"Stop":[{"hooks":[
            {"type":"command","command":owned},
            {"type":"command","command":lookalike}
        ]}]}});
        remove_managed(&mut value, adapter);
        let commands = value["hooks"]["Stop"][0]["hooks"].as_array().unwrap();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0]["command"], lookalike);
    }

    #[test]
    fn install_migrates_exact_legacy_owner_and_redacts_bounded_review() {
        let home = std::env::current_dir()
            .unwrap()
            .join("tmp")
            .join(format!("phase6-hook-migrate-{}", uuid::Uuid::new_v4()));
        let path = home.join(".codex/hooks.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let legacy = "muxflow-host hook ingest --adapter codex # muxflow-managed:v1";
        fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "api_token": "do-not-show",
                "hooks": {"Stop": [{"hooks": [
                    {"type": "command", "command": legacy},
                    {"type": "command", "command": format!("{legacy} lookalike")}
                ]}]}
            }))
            .unwrap(),
        )
        .unwrap();
        let manager = HookManager::for_home(&home);
        let plan = manager
            .review(
                v1::AgentAdapterKind::Codex,
                v1::HookManagementAction::Install,
            )
            .unwrap();
        assert!(!plan.before_preview.contains("do-not-show"));
        assert!(plan.before_preview.contains("<redacted>"));
        manager
            .apply(
                v1::AgentAdapterKind::Codex,
                v1::HookManagementAction::Install,
                &plan.confirmation_token,
            )
            .unwrap();
        let installed = String::from_utf8(fs::read(&path).unwrap()).unwrap();
        assert!(!installed.contains(&format!("\"command\": \"{legacy}\"")));
        assert!(installed.contains(&format!("{legacy} lookalike")));
        assert!(installed.contains("--managed-version 4"));
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn redacted_review_previews_include_the_truncation_marker_within_the_bound() {
        let (preview, truncated) = preview(&serde_json::json!({
            "password": "never-visible",
            "ordinary": "x".repeat(MAX_PREVIEW_BYTES * 2)
        }));
        assert!(truncated);
        assert!(preview.len() <= MAX_PREVIEW_BYTES);
        assert!(preview.ends_with("preview truncated …"));
        assert!(!preview.contains("never-visible"));
    }

    #[test]
    fn redacted_review_never_exposes_nested_private_or_authorization_values() {
        let (preview, truncated) = preview(&serde_json::json!({
            "unrelated": {
                "privateFixtureValue": "phase6-private-value",
                "authorizationHeader": "Bearer phase6-secret",
                "ordinary": "visible-review-value"
            }
        }));
        assert!(!truncated);
        assert!(!preview.contains("phase6-private-value"));
        assert!(!preview.contains("Bearer phase6-secret"));
        assert!(preview.contains("visible-review-value"));
        assert_eq!(preview.matches("<redacted>").count(), 2);
    }

    #[test]
    fn future_owned_version_and_concurrent_apply_fail_closed() {
        let home = std::env::current_dir()
            .unwrap()
            .join("tmp")
            .join(format!("phase6-hook-future-{}", uuid::Uuid::new_v4()));
        let path = home.join(".codex/hooks.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let future = "'/opt/future/muxflow-host' hook ingest --adapter codex --managed-owner muxflow --managed-version 99";
        fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({"hooks":{"Stop":[{"hooks":[{"type":"command","command":future}]}]}})).unwrap(),
        )
        .unwrap();
        let manager = HookManager::for_home(&home);
        assert!(
            manager
                .review(
                    v1::AgentAdapterKind::Codex,
                    v1::HookManagementAction::Install
                )
                .is_err()
        );

        fs::write(&path, b"{}").unwrap();
        let review = manager
            .review(
                v1::AgentAdapterKind::Codex,
                v1::HookManagementAction::Install,
            )
            .unwrap();
        let _lock = ConfigLock::acquire(&path).unwrap();
        assert!(
            manager
                .apply(
                    v1::AgentAdapterKind::Codex,
                    v1::HookManagementAction::Install,
                    &review.confirmation_token
                )
                .is_err()
        );
        fs::remove_dir_all(home).unwrap();
    }

    /// The field configuration this phase exists for: every lifecycle hook
    /// already routed to another tool, one unrelated notifier sharing `Stop`,
    /// matchers on the tool events, and four event names this app does not
    /// manage. Installing beside it must add exactly the managed entries and
    /// change nothing else, twice must change nothing at all, and uninstalling
    /// must give the original file back byte for byte.
    #[test]
    fn install_merges_into_a_real_settings_shape_without_disturbing_its_owner() {
        let home = std::env::current_dir()
            .unwrap()
            .join("tmp")
            .join(format!("phase13-hook-merge-{}", uuid::Uuid::new_v4()));
        let path = home.join(".claude/settings.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = include_bytes!(
            "../../../../../tests/integration/agent-status/fixtures/claude-settings-existing-hooks.json"
        );
        fs::write(&path, original).unwrap();
        let manager = HookManager::for_home(&home);
        let adapter = adapters::adapter(v1::AgentAdapterKind::ClaudeCode).unwrap();

        assert_eq!(
            manager
                .wiring()
                .iter()
                .find(|(_, entry)| entry.adapter_id == "claude-code")
                .unwrap()
                .1
                .state,
            v1::AgentHookWiring::NotWired,
            "hooks that all belong to another tool are not this app's wiring"
        );

        let review = manager
            .review(
                v1::AgentAdapterKind::ClaudeCode,
                v1::HookManagementAction::Install,
            )
            .unwrap();
        assert!(!review.already_current);
        manager
            .apply(
                v1::AgentAdapterKind::ClaudeCode,
                v1::HookManagementAction::Install,
                &review.confirmation_token,
            )
            .unwrap();

        let installed: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let before: Value = serde_json::from_slice(original).unwrap();
        assert_eq!(
            managed_entry_count(&installed, adapter),
            adapter.hook_events().len()
        );
        assert_eq!(
            manager
                .wiring()
                .iter()
                .find(|(_, entry)| entry.adapter_id == "claude-code")
                .unwrap()
                .1
                .state,
            v1::AgentHookWiring::Wired
        );
        // Every unrelated setting, and every pre-existing hook group in every
        // event array, survives untouched and keeps its position.
        for (key, value) in before.as_object().unwrap() {
            if key == "hooks" {
                continue;
            }
            assert_eq!(installed.get(key), Some(value), "{key} was altered");
        }
        for (event, groups) in before["hooks"].as_object().unwrap() {
            let after = installed["hooks"][event].as_array().unwrap();
            let groups = groups.as_array().unwrap();
            assert_eq!(
                &after[..groups.len()],
                &groups[..],
                "{event} lost or reordered a foreign hook"
            );
            let managed = after.len() - groups.len();
            assert_eq!(
                managed,
                usize::from(adapter.hook_events().contains(&event.as_str())),
                "{event} gained the wrong number of managed groups"
            );
        }

        // Idempotence, and only then the reversal.
        let repeated = manager
            .review(
                v1::AgentAdapterKind::ClaudeCode,
                v1::HookManagementAction::Install,
            )
            .unwrap();
        assert!(repeated.already_current);
        let bytes_before_repeat = fs::read(&path).unwrap();
        manager
            .apply(
                v1::AgentAdapterKind::ClaudeCode,
                v1::HookManagementAction::Install,
                &repeated.confirmation_token,
            )
            .unwrap();
        assert_eq!(
            fs::read(&path).unwrap(),
            bytes_before_repeat,
            "a second install rewrote the file"
        );

        let uninstall = manager
            .review(
                v1::AgentAdapterKind::ClaudeCode,
                v1::HookManagementAction::Uninstall,
            )
            .unwrap();
        manager
            .apply(
                v1::AgentAdapterKind::ClaudeCode,
                v1::HookManagementAction::Uninstall,
                &uninstall.confirmation_token,
            )
            .unwrap();
        let removed: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            removed, before,
            "uninstall did not restore the original configuration"
        );
        assert_eq!(fs::read(backup_path(&path)).unwrap(), original);
        fs::remove_dir_all(home).unwrap();
    }

    /// The override names one adapter, and relocating Claude Code's settings
    /// must not silently relocate Codex's — a single path applied to every
    /// adapter had Codex's wiring read out of Claude Code's file.
    #[test]
    fn a_settings_override_moves_only_the_adapter_it_names() {
        let home = std::env::current_dir()
            .unwrap()
            .join("tmp")
            .join(format!("phase13-hook-override-{}", uuid::Uuid::new_v4()));
        let elsewhere = home.join("fixture/settings.json");
        fs::create_dir_all(elsewhere.parent().unwrap()).unwrap();
        fs::write(&elsewhere, b"{}").unwrap();
        let manager = HookManager::with_overrides(
            Some(home.clone()),
            Some(("claude-code", elsewhere.clone())),
        )
        .unwrap();
        let path = |id: &str| {
            manager
                .wiring()
                .into_iter()
                .find(|(_, entry)| entry.adapter_id == id)
                .unwrap()
                .1
                .config_path
        };
        assert_eq!(path("claude-code"), elsewhere);
        assert_eq!(path("codex"), home.join(".codex/hooks.json"));
        fs::remove_dir_all(home).unwrap();
    }

    /// Wiring is an observation with four failure modes that must not be
    /// confused: the agent is not here at all, nothing installed, something
    /// installed but incomplete, and a configuration nobody could read. Only
    /// the middle two invite an install.
    #[test]
    fn wiring_separates_absent_incomplete_and_unreadable_configuration() {
        let home = std::env::current_dir()
            .unwrap()
            .join("tmp")
            .join(format!("phase13-hook-wiring-{}", uuid::Uuid::new_v4()));
        let path = home.join(".claude/settings.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let manager = HookManager::for_home(&home);
        let state = |manager: &HookManager| {
            manager
                .wiring()
                .into_iter()
                .find(|(_, entry)| entry.adapter_id == "claude-code")
                .unwrap()
                .1
        };
        assert_eq!(state(&manager).state, v1::AgentHookWiring::NotWired);

        // One managed entry for one event: the remaining transitions can never
        // arrive, which is not the same as being wired.
        let adapter = adapters::adapter(v1::AgentAdapterKind::ClaudeCode).unwrap();
        let command = adapter.hook_command(Path::new("/opt/muxflow/bin/muxflow-host"));
        fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "hooks": {"Stop": [{"hooks": [{"type": "command", "command": command}]}]}
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(state(&manager).state, v1::AgentHookWiring::Partial);

        fs::write(&path, b"{ not json").unwrap();
        let unreadable = state(&manager);
        assert_eq!(unreadable.state, v1::AgentHookWiring::Unavailable);
        assert!(!unreadable.detail.is_empty(), "unavailable must say why");
        fs::remove_dir_all(home).unwrap();
    }

    /// An agent that is not installed here is not a gap to be filled. Reported
    /// as unwired, the desktop offered to "set up" a vendor the user has never
    /// run — and accepting created its configuration directory and file.
    #[test]
    fn an_agent_that_is_not_on_this_host_is_absent_rather_than_unwired() {
        let home = std::env::current_dir()
            .unwrap()
            .join("tmp")
            .join(format!("phase13-hook-absent-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&home).unwrap();
        let manager = HookManager::for_home(&home);
        let codex = adapters::adapter(v1::AgentAdapterKind::Codex).unwrap();
        let state = |id: &str| {
            manager
                .wiring()
                .into_iter()
                .find(|(_, entry)| entry.adapter_id == id)
                .unwrap()
                .1
                .state
        };
        // Nothing at all: no config, no directory, nothing on the search path.
        // The path is passed rather than read so this does not depend on
        // whether the machine running the test happens to have Codex.
        assert!(!manager.agent_is_present_in(codex, None));
        assert!(!manager.agent_is_present_in(codex, Some(home.as_os_str().to_owned())));

        // The directory alone is enough to prove the agent has been here.
        fs::create_dir_all(home.join(".codex")).unwrap();
        assert!(manager.agent_is_present_in(codex, None));
        assert_eq!(state("codex"), v1::AgentHookWiring::NotWired);

        // And so is a configuration file with no managed entries in it.
        fs::write(home.join(".codex/hooks.json"), b"{}").unwrap();
        assert_eq!(state("codex"), v1::AgentHookWiring::NotWired);

        // And an executable on the search path, with no configuration at all.
        let bin = home.join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join(codex.executable()), b"#!/bin/sh\n").unwrap();
        fs::remove_dir_all(home.join(".codex")).unwrap();
        assert!(!manager.agent_is_present_in(codex, None));
        assert!(manager.agent_is_present_in(codex, Some(bin.as_os_str().to_owned())));
        fs::remove_dir_all(home).unwrap();
    }

    /// The cache is on the snapshot path, so it has to answer immediately after
    /// an install rather than after a timeout — a fingerprint, not a clock.
    #[test]
    fn the_wiring_cache_re_reads_a_configuration_the_moment_it_changes() {
        let home = std::env::current_dir()
            .unwrap()
            .join("tmp")
            .join(format!("phase13-wiring-cache-{}", uuid::Uuid::new_v4()));
        let path = home.join(".claude/settings.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let manager = HookManager::for_home(&home);
        let mut cache = WiringCache::default();
        let nothing_running = Default::default();
        let state = |wiring: &[ObservedAdapter]| {
            wiring
                .iter()
                .find(|(_, entry)| entry.adapter_id == "claude-code")
                .unwrap()
                .1
                .state
        };
        assert_eq!(
            state(&WiringCache::memoized(
                &mut cache.observed,
                &manager,
                &nothing_running
            )),
            v1::AgentHookWiring::NotWired
        );

        let adapter = adapters::adapter(v1::AgentAdapterKind::ClaudeCode).unwrap();
        let review = manager
            .review(
                v1::AgentAdapterKind::ClaudeCode,
                v1::HookManagementAction::Install,
            )
            .unwrap();
        manager
            .apply(
                v1::AgentAdapterKind::ClaudeCode,
                v1::HookManagementAction::Install,
                &review.confirmation_token,
            )
            .unwrap();
        assert_eq!(
            state(&WiringCache::memoized(
                &mut cache.observed,
                &manager,
                &nothing_running
            )),
            v1::AgentHookWiring::Wired,
            "an install a moment ago must not be hidden behind a cached answer"
        );
        // And an unchanged configuration is not re-parsed into a new answer.
        assert_eq!(
            state(&WiringCache::memoized(
                &mut cache.observed,
                &manager,
                &nothing_running
            )),
            state(&WiringCache::memoized(
                &mut cache.observed,
                &manager,
                &nothing_running
            )),
            "a stable configuration must produce a stable observation"
        );
        assert_eq!(adapter.id(), "claude-code");
        fs::remove_dir_all(home).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn exchange_cas_rolls_back_a_noncooperating_racing_edit() {
        let dir = std::env::current_dir()
            .unwrap()
            .join("tmp")
            .join(format!("phase6-hook-cas-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hooks.json");
        let temporary = dir.join("new.tmp");
        fs::write(&path, b"racing editor bytes").unwrap();
        fs::write(&temporary, b"our proposed bytes").unwrap();
        assert!(exchange_reviewed(&temporary, &path, b"reviewed old bytes").is_err());
        assert_eq!(fs::read(&path).unwrap(), b"racing editor bytes");
        assert_eq!(fs::read(&temporary).unwrap(), b"our proposed bytes");
        fs::remove_dir_all(dir).unwrap();
    }
}
