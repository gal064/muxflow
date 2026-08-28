use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::Mutex,
};

use serde::{Deserialize, Serialize};
use tauri::State;
use uuid::Uuid;

const APP_STATE_SCHEMA_VERSION: u32 = 1;
const MAX_APP_TABS: usize = 10_000;
const MAX_TEXT_FIELD_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AppTabRecord {
    pub id: String,
    pub host_profile_id: String,
    #[serde(default)]
    pub server_identity: String,
    pub session_id: String,
    pub session_name: String,
    pub kind: AppTabKind,
    pub resource: String,
    #[serde(default)]
    pub root_path: Option<String>,
    #[serde(default)]
    pub root_token: Option<String>,
    /// VS Code's preview tab. `serde(default)` because every file written
    /// before preview tabs existed lacks it, and a required field here is
    /// exactly the mismatch that once made every save fail.
    #[serde(default)]
    pub preview: Option<bool>,
    #[serde(default)]
    pub view_mode: Option<AppTabViewMode>,
    #[serde(default)]
    pub git_repository_id: Option<String>,
    #[serde(default)]
    pub git_path: Option<String>,
    #[serde(default)]
    pub git_original_path: Option<String>,
    #[serde(default)]
    pub git_target: Option<GitDiffTarget>,
    #[serde(default)]
    pub git_status_generation: Option<String>,
    #[serde(default)]
    pub git_source_generation: Option<String>,
    pub title: String,
    pub order: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AppTabViewMode {
    Source,
    Preview,
    Split,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AppTabKind {
    File,
    Markdown,
    GitDiff,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum GitDiffTarget {
    Staged,
    Unstaged,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceUiRecord {
    pub host_profile_id: String,
    #[serde(default)]
    pub server_identity: String,
    pub session_id: String,
    pub session_name: String,
    pub selected_app_tab_id: Option<String>,
}

/// The shell preferences the frontend owns.
///
/// Every field is `#[serde(default)]`, and that is a contract rather than a
/// convenience. This struct is the *storage* end of a shape declared in
/// TypeScript (`features/shell/types.ts`, `ShellState`); the two are written by
/// hand, so a field the frontend adds or drops must not be able to make the
/// whole save fail. It could before: Phase 11 replaced the shell preferences
/// wholesale and left three required fields behind here, so every
/// `save_app_state` was rejected with `missing field \`explorerSurface\`` and
/// the app silently stopped persisting open tabs, workspace selection,
/// shortcut overrides and window geometry. `app_state_contract` below is the
/// test that would have caught it.
///
/// Fields removed by the frontend are simply dropped: serde ignores unknown
/// keys, so a file written by the previous build still loads, with the new
/// preferences at their defaults. That is why `schema_version` stays at 1.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ShellPreferences {
    #[serde(default)]
    pub panel_surface: PanelSurface,
    #[serde(default)]
    pub sidebar_collapsed: bool,
    #[serde(default)]
    pub sidebar_width: Option<f64>,
    #[serde(default)]
    pub panel_open: bool,
    #[serde(default)]
    pub panel_width: Option<f64>,
    #[serde(default)]
    pub agent_sort: AgentSortMode,
    #[serde(default)]
    pub agents_section_ratio: Option<f64>,
    #[serde(default)]
    pub agent_state_glyphs: bool,
    #[serde(default)]
    pub compact_workspaces: bool,
    /// The workspace list's one filter: pinned workspaces only.
    #[serde(default)]
    pub pinned_only: bool,
    #[serde(default)]
    pub terminal_screen_reader: bool,
    #[serde(default)]
    pub copy_on_select: bool,
    #[serde(default)]
    pub terminal_application_clipboard: bool,
    #[serde(default)]
    pub terminal_font_size: Option<u8>,
    /// The mode a newly opened Markdown tab starts in. `None` is a save written
    /// before the setting existed, which is the same thing as "split".
    #[serde(default)]
    pub default_markdown_view: Option<AppTabViewMode>,
    #[serde(default)]
    pub window_geometry: Option<WindowGeometry>,
}

/// What a new workspace on one host profile starts with.
///
/// Both halves are per host and both are optional, because a filesystem path
/// and a shell command are statements about one machine. Absent means the
/// behaviour this app had before the setting existed.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceDefaults {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub startup_command: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WindowGeometry {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub maximized: bool,
    #[serde(default)]
    pub scale_factor_milli: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CommandPreferences {
    #[serde(default)]
    pub shortcut_overrides: HashMap<String, Option<String>>,
}

/// Which half of the right panel is showing when it is open.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PanelSurface {
    #[default]
    Files,
    Git,
}

/// The agents section's ordering: workspace order, or attention order.
///
/// The two orderings were named `grouped` and `priority` before they were
/// named after what they sort by. An unknown variant is a hard deserialization
/// error, not a defaulted field, so both old names are still accepted here:
/// without the aliases, a file written by the previous build would make every
/// `load_app_state` fail — and, worse, `save_app_state` would have rejected the
/// frontend's new names, silently freezing every saved tab and preference.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AgentSortMode {
    #[serde(alias = "priority")]
    Status,
    #[default]
    #[serde(alias = "grouped")]
    Workspace,
}

// No `Eq`: the shell's sidebar width and agents-section ratio are fractions.
/// Whether the user has answered "set up this host" for one host profile.
///
/// Stored per host rather than globally: consent to change configuration files
/// on a laptop says nothing about a shared build box, and the prompt is
/// one-time per host precisely because re-asking is how a consent prompt turns
/// into something people click through.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum HostSetupDecision {
    Accepted,
    Declined,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PersistedAppState {
    pub schema_version: u32,
    pub app_tabs: Vec<AppTabRecord>,
    pub workspace_ui: Vec<WorkspaceUiRecord>,
    pub shell: ShellPreferences,
    #[serde(default)]
    pub commands: CommandPreferences,
    #[serde(default)]
    pub host_setup: HashMap<String, HostSetupDecision>,
    #[serde(default)]
    pub workspace_defaults: BTreeMap<String, WorkspaceDefaults>,
}

impl Default for PersistedAppState {
    fn default() -> Self {
        Self {
            schema_version: APP_STATE_SCHEMA_VERSION,
            app_tabs: Vec::new(),
            workspace_ui: Vec::new(),
            shell: ShellPreferences::default(),
            commands: CommandPreferences::default(),
            host_setup: HashMap::new(),
            workspace_defaults: BTreeMap::new(),
        }
    }
}

pub struct AppStateStore {
    path: PathBuf,
    value: Mutex<PersistedAppState>,
    recovery_error: Mutex<Option<String>>,
    /// Serialises `save_app_state`, whose file write and cache update are now
    /// separated by an await.
    write_lock: tauri::async_runtime::Mutex<()>,
}

impl AppStateStore {
    pub fn load(path: PathBuf) -> Self {
        let loaded = load_value(&path).and_then(|value| {
            validate(&value)?;
            Ok(value)
        });
        let (value, recovery_error) = match loaded {
            Ok(value) => (value, None),
            Err(error) => (PersistedAppState::default(), Some(error)),
        };
        Self {
            path,
            value: Mutex::new(value),
            recovery_error: Mutex::new(recovery_error),
            write_lock: tauri::async_runtime::Mutex::new(()),
        }
    }
}

fn load_value(path: &Path) -> Result<PersistedAppState, String> {
    if !path.exists() {
        return Ok(PersistedAppState::default());
    }
    let bytes = fs::read(path).map_err(|error| error.to_string())?;
    let value: PersistedAppState =
        serde_json::from_slice(&bytes).map_err(|error| format!("invalid app state: {error}"))?;
    if value.schema_version != APP_STATE_SCHEMA_VERSION {
        return Err(format!(
            "unsupported app-state schema {}; expected {}",
            value.schema_version, APP_STATE_SCHEMA_VERSION
        ));
    }
    Ok(value)
}

fn write_private_atomic(path: &Path, value: &PersistedAppState) -> Result<(), String> {
    let parent = path.parent().ok_or("app-state path has no parent")?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
        .map_err(|error| error.to_string())?;
    let data = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    let temporary = path.with_extension(format!("{}.partial", Uuid::new_v4()));
    fs::write(&temporary, data).map_err(|error| error.to_string())?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))
        .map_err(|error| error.to_string())?;
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error.to_string());
    }
    Ok(())
}

fn validate(value: &PersistedAppState) -> Result<(), String> {
    if value.schema_version != APP_STATE_SCHEMA_VERSION {
        return Err("app-state schema version is not writable by this desktop".into());
    }
    if value.app_tabs.len() > MAX_APP_TABS {
        return Err(format!("app state exceeds {MAX_APP_TABS} open tabs"));
    }
    if let Some(geometry) = value.shell.window_geometry
        && (geometry.width < 320
            || geometry.height < 240
            || geometry.width > 32_768
            || geometry.height > 32_768
            || geometry
                .scale_factor_milli
                .is_some_and(|scale| !(500..=8_000).contains(&scale)))
    {
        return Err("invalid saved window geometry".into());
    }
    if value.commands.shortcut_overrides.len() > 256 {
        return Err("too many keyboard shortcut overrides".into());
    }
    for (command, binding) in &value.commands.shortcut_overrides {
        validate_text("shortcut command", command, false)?;
        if let Some(binding) = binding {
            validate_text("shortcut binding", binding, false)?;
        }
    }
    if value.host_setup.len() > 1_024 {
        return Err("too many recorded host setup decisions".into());
    }
    for host_profile_id in value.host_setup.keys() {
        validate_text("host setup profile ID", host_profile_id, false)?;
    }
    if value.workspace_defaults.len() > 1_024 {
        return Err("too many recorded workspace defaults".into());
    }
    for (host_profile_id, defaults) in &value.workspace_defaults {
        validate_text("workspace defaults profile ID", host_profile_id, false)?;
        if let Some(directory) = defaults.directory.as_deref() {
            validate_text("workspace start directory", directory, false)?;
        }
        if let Some(startup_command) = defaults.startup_command.as_deref() {
            validate_text("workspace startup command", startup_command, false)?;
        }
    }
    let mut ids = HashSet::new();
    for tab in &value.app_tabs {
        validate_text("tab ID", &tab.id, false)?;
        validate_text("host profile ID", &tab.host_profile_id, false)?;
        validate_text("server identity", &tab.server_identity, true)?;
        validate_text("session name", &tab.session_name, false)?;
        validate_text("tab resource", &tab.resource, false)?;
        if let Some(root_path) = tab.root_path.as_deref() {
            validate_text("tab root path", root_path, false)?;
            if !std::path::Path::new(root_path).is_absolute() {
                return Err("tab root path must be absolute".into());
            }
        }
        if let Some(root_token) = tab.root_token.as_deref() {
            validate_text("tab root token", root_token, false)?;
        }
        if tab.root_path.is_some() != tab.root_token.is_some() {
            return Err("tab root path and token must be persisted together".into());
        }
        for (label, field) in [
            ("Git repository ID", tab.git_repository_id.as_deref()),
            ("Git path", tab.git_path.as_deref()),
            ("Git original path", tab.git_original_path.as_deref()),
            (
                "Git source generation",
                tab.git_source_generation.as_deref(),
            ),
        ] {
            if let Some(field) = field {
                validate_text(label, field, false)?;
            }
        }
        if let Some(generation) = tab.git_status_generation.as_deref() {
            validate_text("Git status generation", generation, false)?;
            if !generation.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err("Git status generation must be a decimal u64".into());
            }
        }
        let has_git_identity = tab.git_repository_id.is_some()
            || tab.git_path.is_some()
            || tab.git_original_path.is_some()
            || tab.git_target.is_some()
            || tab.git_status_generation.is_some()
            || tab.git_source_generation.is_some();
        if has_git_identity
            && (tab.kind != AppTabKind::GitDiff
                || tab.git_repository_id.is_none()
                || tab.git_path.is_none()
                || tab.git_target.is_none()
                || tab.git_status_generation.is_none()
                || tab.git_source_generation.is_none())
        {
            return Err("Git diff tab identity is incomplete".into());
        }
        validate_text("tab title", &tab.title, false)?;
        if !tab.session_id.is_empty() {
            validate_tmux_session_id(&tab.session_id)?;
        }
        if !ids.insert((tab.host_profile_id.as_str(), tab.id.as_str())) {
            return Err(format!(
                "duplicate app tab ID {} for host {}",
                tab.id, tab.host_profile_id
            ));
        }
    }
    let known_tabs: HashMap<_, _> = value
        .app_tabs
        .iter()
        .map(|tab| ((tab.host_profile_id.as_str(), tab.id.as_str()), tab))
        .collect();
    let mut workspaces = HashSet::new();
    for workspace in &value.workspace_ui {
        validate_text("host profile ID", &workspace.host_profile_id, false)?;
        validate_text("server identity", &workspace.server_identity, true)?;
        validate_text("session name", &workspace.session_name, false)?;
        if !workspace.session_id.is_empty() {
            validate_tmux_session_id(&workspace.session_id)?;
        }
        if !workspaces.insert((
            workspace.host_profile_id.as_str(),
            workspace.server_identity.as_str(),
            workspace.session_id.as_str(),
            workspace.session_name.as_str(),
        )) {
            return Err("duplicate workspace UI state".into());
        }
        if let Some(tab_id) = workspace.selected_app_tab_id.as_deref() {
            let Some(tab) = known_tabs.get(&(workspace.host_profile_id.as_str(), tab_id)) else {
                return Err(format!("selected app tab {tab_id} does not exist"));
            };
            if tab.server_identity != workspace.server_identity
                || tab.session_id != workspace.session_id
                || tab.session_name != workspace.session_name
            {
                return Err(format!(
                    "selected app tab {tab_id} belongs to a different workspace"
                ));
            }
        }
    }
    Ok(())
}

fn validate_text(label: &str, value: &str, allow_empty: bool) -> Result<(), String> {
    if (!allow_empty && value.is_empty())
        || value.len() > MAX_TEXT_FIELD_BYTES
        || value.contains('\0')
    {
        return Err(format!("invalid {label}"));
    }
    Ok(())
}

fn validate_tmux_session_id(value: &str) -> Result<(), String> {
    if value.strip_prefix('$').is_some_and(|suffix| {
        !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
    }) {
        Ok(())
    } else {
        Err(format!("invalid tmux session ID {value}"))
    }
}

#[tauri::command]
pub fn load_app_state(store: State<'_, AppStateStore>) -> Result<PersistedAppState, String> {
    if let Some(error) = store.recovery_error.lock().unwrap().as_ref() {
        return Err(format!("Saved shell state requires recovery: {error}"));
    }
    Ok(store.value.lock().unwrap().clone())
}

/// Async: shell state is saved on ordinary interactions such as opening a tab,
/// and an atomic write plus fsync on the WebView's main thread stalls the whole
/// UI for the length of that disk round trip.
#[tauri::command]
pub async fn save_app_state(
    state: PersistedAppState,
    store: State<'_, AppStateStore>,
) -> Result<(), String> {
    if store.recovery_error.lock().unwrap().is_some() {
        return Err("saved shell state is write-frozen until explicit reset".into());
    }
    validate(&state)?;
    // One save at a time. The await between writing the file and updating the
    // cache is a window two concurrent saves could interleave in, leaving the
    // file holding one state and the cache another — and shell state is saved
    // on ordinary interactions, so concurrent saves are the normal case, not an
    // exotic one.
    let _serialized = store.write_lock.lock().await;
    let path = store.path.clone();
    let persisted = state.clone();
    tauri::async_runtime::spawn_blocking(move || write_private_atomic(&path, &persisted))
        .await
        .map_err(|error| format!("shell state write task failed: {error}"))??;
    *store.value.lock().unwrap() = state;
    Ok(())
}

#[tauri::command]
pub fn reset_app_state(store: State<'_, AppStateStore>) -> Result<PersistedAppState, String> {
    let default = PersistedAppState::default();
    if store.path.exists() {
        let backup = store
            .path
            .with_extension(format!("invalid-{}.json", Uuid::new_v4()));
        fs::rename(&store.path, &backup).map_err(|error| {
            format!(
                "could not preserve invalid state at {}: {error}",
                backup.display()
            )
        })?;
    }
    write_private_atomic(&store.path, &default)?;
    *store.value.lock().unwrap() = default.clone();
    *store.recovery_error.lock().unwrap() = None;
    Ok(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_state() -> PersistedAppState {
        PersistedAppState {
            schema_version: APP_STATE_SCHEMA_VERSION,
            app_tabs: vec![AppTabRecord {
                id: "tab-1".into(),
                host_profile_id: "local".into(),
                server_identity: "server-a".into(),
                session_id: "$1".into(),
                session_name: "project".into(),
                kind: AppTabKind::Markdown,
                resource: "/work/README.md".into(),
                root_path: Some("/work".into()),
                root_token: Some("root-token".into()),
                preview: Some(true),
                view_mode: Some(AppTabViewMode::Split),
                git_repository_id: None,
                git_path: None,
                git_original_path: None,
                git_target: None,
                git_status_generation: None,
                git_source_generation: None,
                title: "README.md".into(),
                order: 0,
            }],
            workspace_ui: vec![WorkspaceUiRecord {
                host_profile_id: "local".into(),
                server_identity: "server-a".into(),
                session_id: "$1".into(),
                session_name: "project".into(),
                selected_app_tab_id: Some("tab-1".into()),
            }],
            shell: ShellPreferences {
                panel_surface: PanelSurface::Git,
                sidebar_collapsed: true,
                sidebar_width: Some(260.0),
                panel_open: true,
                panel_width: Some(320.0),
                agent_sort: AgentSortMode::Status,
                agents_section_ratio: Some(0.42),
                agent_state_glyphs: true,
                compact_workspaces: true,
                pinned_only: true,
                terminal_screen_reader: false,
                copy_on_select: false,
                terminal_application_clipboard: false,
                terminal_font_size: Some(13),
                default_markdown_view: Some(AppTabViewMode::Preview),
                window_geometry: Some(WindowGeometry {
                    x: 20,
                    y: 30,
                    width: 1200,
                    height: 800,
                    maximized: false,
                    scale_factor_milli: Some(2_000),
                }),
            },
            commands: CommandPreferences {
                shortcut_overrides: HashMap::from([("window.new".into(), Some("Ctrl+T".into()))]),
            },
            host_setup: HashMap::from([("local".into(), HostSetupDecision::Accepted)]),
            workspace_defaults: BTreeMap::from([(
                "local".into(),
                WorkspaceDefaults {
                    directory: Some("/work/projects".into()),
                    startup_command: Some("git status".into()),
                },
            )]),
        }
    }

    #[test]
    fn app_state_round_trips_atomically_with_private_permissions() {
        let root = std::env::temp_dir().join(format!("ade-app-state-{}", Uuid::new_v4()));
        let path = root.join("app-state.json");
        let store = AppStateStore::load(path.clone());
        *store.value.lock().unwrap() = sample_state();
        write_private_atomic(&store.path, &store.value.lock().unwrap()).unwrap();

        let restored = AppStateStore::load(path.clone());
        assert_eq!(*restored.value.lock().unwrap(), sample_state());
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn git_diff_identity_round_trips_without_losing_opaque_paths() {
        let mut state = sample_state();
        state.app_tabs.push(AppTabRecord {
            id: "git-tab".into(),
            host_profile_id: "local".into(),
            server_identity: "server-a".into(),
            session_id: "$1".into(),
            session_name: "project".into(),
            kind: AppTabKind::GitDiff,
            resource: "staged:new name".into(),
            root_path: Some("/work".into()),
            root_token: Some("root-token".into()),
            preview: None,
            view_mode: None,
            git_repository_id: Some("repo-identity".into()),
            git_path: Some("bmV3IG5hbWU=".into()),
            git_original_path: Some("b2xkIG5hbWU=".into()),
            git_target: Some(GitDiffTarget::Staged),
            git_status_generation: Some("18446744073709551615".into()),
            git_source_generation: Some("source-generation".into()),
            title: "new name (staged)".into(),
            order: 1,
        });
        validate(&state).unwrap();
        let encoded = serde_json::to_vec(&state).unwrap();
        let restored: PersistedAppState = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(restored.app_tabs[1], state.app_tabs[1]);
    }

    /// The pins moved to the host, where they belong to the tmux server rather
    /// than to this installation. A file written by the build that kept them
    /// here must still load: the records are simply not read.
    #[test]
    fn pin_records_written_by_the_previous_build_are_ignored() {
        let legacy: PersistedAppState = serde_json::from_value(serde_json::json!({
            "schemaVersion": 1, "appTabs": [], "workspaceUi": [], "shell": {},
            "pinnedWorkspaces": [{
                "hostProfileId": "local", "serverIdentity": "server-a",
                "sessionId": "$1", "sessionName": "project", "pinnedAt": 1
            }],
            "pinnedTabs": [{
                "hostProfileId": "local", "serverIdentity": "server-a",
                "sessionId": "$1", "tabId": "@3", "pinnedAt": 2
            }]
        }))
        .unwrap();
        assert_eq!(legacy, PersistedAppState::default());
        validate(&legacy).unwrap();
    }

    #[test]
    fn app_state_rejects_dangling_selection_and_duplicate_tab_ids() {
        let mut value = sample_state();
        value.workspace_ui[0].selected_app_tab_id = Some("missing".into());
        assert!(validate(&value).unwrap_err().contains("does not exist"));

        let mut value = sample_state();
        value.app_tabs.push(value.app_tabs[0].clone());
        assert!(validate(&value).unwrap_err().contains("duplicate app tab"));
    }

    #[test]
    fn window_geometry_accepts_hidpi_scale_and_rejects_implausible_values() {
        let value = sample_state();
        validate(&value).unwrap();

        let mut invalid = value.clone();
        invalid
            .shell
            .window_geometry
            .as_mut()
            .unwrap()
            .scale_factor_milli = Some(20_000);
        assert!(validate(&invalid).unwrap_err().contains("window geometry"));

        let legacy = serde_json::json!({
            "x": 1, "y": 2, "width": 800, "height": 500, "maximized": false
        });
        let restored: WindowGeometry = serde_json::from_value(legacy).unwrap();
        assert_eq!(restored.scale_factor_milli, None);
    }

    /// The storage end of the `save_app_state` contract.
    ///
    /// `PersistedAppState` is declared twice — here and in
    /// `src/features/shell/types.ts` — and hand-copied between them. The
    /// fixture below is read by both ends; `appStateContract.test.ts` asserts
    /// the frontend's key set against it, and this asserts that everything in
    /// it deserializes and that this side invents no field the frontend never
    /// sends.
    ///
    /// The break it exists to prevent, measured on the packaged app: the
    /// frontend replaced the shell preferences wholesale, three fields stayed
    /// required here, and every save was refused with
    /// `missing field \`explorerSurface\`` — open tabs, workspace selection,
    /// shortcut overrides and window geometry all silently stopped persisting.
    #[test]
    fn app_state_contract_matches_the_frontend_shape() {
        const CONTRACT: &str =
            include_str!("../../src/features/shell/persistedAppState.contract.json");
        let value: PersistedAppState =
            serde_json::from_str(CONTRACT).expect("the frontend's own payload must deserialize");
        assert_eq!(value.shell.panel_surface, PanelSurface::Git);
        assert_eq!(value.shell.agent_sort, AgentSortMode::Status);
        assert_eq!(value.shell.sidebar_width, Some(260.0));
        assert_eq!(value.shell.panel_width, Some(320.0));
        assert_eq!(value.shell.agents_section_ratio, Some(0.42));
        assert_eq!(value.shell.terminal_font_size, Some(17));
        assert!(value.shell.sidebar_collapsed && value.shell.panel_open);
        assert!(
            value.shell.agent_state_glyphs
                && value.shell.compact_workspaces
                && value.shell.pinned_only
                && value.shell.terminal_screen_reader
                && value.shell.copy_on_select
                && value.shell.terminal_application_clipboard
        );
        assert!(value.shell.window_geometry.is_some());
        assert_eq!(
            value.host_setup.get("local"),
            Some(&HostSetupDecision::Accepted)
        );
        assert_eq!(
            value.host_setup.get("ssh-remote-linux"),
            Some(&HostSetupDecision::Declined)
        );
        assert_eq!(
            value.shell.default_markdown_view,
            Some(AppTabViewMode::Preview)
        );
        // Per host, and never merged: the local directory and the remote one
        // are two different machines' filesystems.
        assert_eq!(
            value.workspace_defaults.get("local"),
            Some(&WorkspaceDefaults {
                directory: Some("/work/projects".into()),
                startup_command: Some("git status".into()),
            })
        );
        assert_eq!(
            value
                .workspace_defaults
                .get("ssh-remote-linux")
                .and_then(|defaults| defaults.directory.as_deref()),
            Some("/srv/checkout")
        );
        validate(&value).expect("the frontend's own payload must validate");

        // Nothing may be stored that the frontend does not send, and nothing the
        // frontend sends may be silently dropped. Checked for every struct that
        // crosses, not just the one that broke: `appTabs` carries nineteen
        // fields and `workspaceUi` five, and either could lose one the same way.
        let expected: serde_json::Value = serde_json::from_str(CONTRACT).unwrap();
        let stored = serde_json::to_value(&value).unwrap();
        let keys = |value: &serde_json::Value| {
            let mut names: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
            names.sort();
            names
        };
        assert_eq!(keys(&stored["shell"]), keys(&expected["shell"]), "shell");
        assert_eq!(
            keys(&stored["appTabs"][0]),
            keys(&expected["appTabs"][0]),
            "appTabs"
        );
        assert_eq!(
            keys(&stored["workspaceUi"][0]),
            keys(&expected["workspaceUi"][0]),
            "workspaceUi"
        );
        assert_eq!(
            keys(&stored["commands"]),
            keys(&expected["commands"]),
            "commands"
        );
        assert_eq!(
            keys(&stored["workspaceDefaults"]["local"]),
            keys(&expected["workspaceDefaults"]["local"]),
            "workspaceDefaults"
        );
        // And the envelope itself, so a whole section cannot go missing.
        assert_eq!(
            keys(&stored),
            keys(
                &expected
                    .as_object()
                    .unwrap()
                    .iter()
                    .filter(|(name, _)| !name.starts_with('_'))
                    .map(|(name, value)| (name.clone(), value.clone()))
                    .collect::<serde_json::Map<_, _>>()
                    .into()
            )
        );
    }

    /// A file written by the build before Phase 11 must still load, with the
    /// preferences it never heard of at their defaults. The schema version
    /// deliberately did not move for a shell-preferences change.
    #[test]
    fn shell_preferences_written_by_the_previous_build_still_load() {
        let legacy = serde_json::json!({
            "schemaVersion": 1,
            "appTabs": [],
            "workspaceUi": [],
            "shell": {
                "explorerSurface": "git",
                "explorerCollapsed": true,
                "agentSidebarCollapsed": true,
                "windowGeometry": { "x": 1, "y": 2, "width": 900, "height": 700, "maximized": false }
            }
        });
        let value: PersistedAppState = serde_json::from_value(legacy).unwrap();
        assert_eq!(value.shell.panel_surface, PanelSurface::Files);
        assert_eq!(value.shell.agent_sort, AgentSortMode::Workspace);
        assert!(!value.shell.sidebar_collapsed);
        assert_eq!(value.shell.window_geometry.unwrap().width, 900);
    }

    /// The agent ordering was renamed, not changed. A file naming an ordering
    /// the way the previous build wrote it must still load — as the *same*
    /// ordering, not as the default. An unknown enum variant is a hard error
    /// in serde, so without the aliases this is a load failure that freezes
    /// every saved tab and preference behind the recovery path.
    #[test]
    fn renamed_agent_orderings_still_load_under_their_previous_names() {
        let load = |sort: &str| {
            let value: PersistedAppState = serde_json::from_value(serde_json::json!({
                "schemaVersion": 1,
                "appTabs": [],
                "workspaceUi": [],
                "shell": { "agentSort": sort },
            }))
            .unwrap_or_else(|error| panic!("agentSort {sort} must load: {error}"));
            value.shell.agent_sort
        };
        assert_eq!(load("priority"), AgentSortMode::Status);
        assert_eq!(load("grouped"), AgentSortMode::Workspace);
        assert_eq!(load("status"), AgentSortMode::Status);
        assert_eq!(load("workspace"), AgentSortMode::Workspace);
        // And what is written back is the current name, never the old one.
        assert_eq!(
            serde_json::to_value(AgentSortMode::Status).unwrap(),
            serde_json::json!("status")
        );
    }

    #[test]
    fn future_schema_is_recovery_frozen_without_bricking_startup_or_overwrite() {
        let root = std::env::temp_dir().join(format!("ade-app-state-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("app-state.json");
        fs::write(&path, r#"{"schemaVersion":99,"appTabs":[],"workspaceUi":[],"shell":{"explorerSurface":"explorer","explorerCollapsed":false,"agentSidebarCollapsed":false}}"#).unwrap();
        let original = fs::read(&path).unwrap();
        let store = AppStateStore::load(path.clone());
        assert!(
            store
                .recovery_error
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .contains("unsupported app-state schema")
        );
        assert_eq!(fs::read(path).unwrap(), original);
        fs::remove_dir_all(root).unwrap();
    }
}
