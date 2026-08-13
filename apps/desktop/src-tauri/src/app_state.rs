use std::{
    collections::{HashMap, HashSet},
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ShellPreferences {
    pub explorer_surface: ExplorerSurface,
    pub explorer_collapsed: bool,
    pub agent_sidebar_collapsed: bool,
    #[serde(default)]
    pub window_geometry: Option<WindowGeometry>,
}

impl Default for ShellPreferences {
    fn default() -> Self {
        Self {
            explorer_surface: ExplorerSurface::Explorer,
            explorer_collapsed: false,
            agent_sidebar_collapsed: false,
            window_geometry: None,
        }
    }
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ExplorerSurface {
    Explorer,
    Git,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PersistedAppState {
    pub schema_version: u32,
    pub app_tabs: Vec<AppTabRecord>,
    pub workspace_ui: Vec<WorkspaceUiRecord>,
    pub shell: ShellPreferences,
    #[serde(default)]
    pub commands: CommandPreferences,
}

impl Default for PersistedAppState {
    fn default() -> Self {
        Self {
            schema_version: APP_STATE_SCHEMA_VERSION,
            app_tabs: Vec::new(),
            workspace_ui: Vec::new(),
            shell: ShellPreferences::default(),
            commands: CommandPreferences::default(),
        }
    }
}

pub struct AppStateStore {
    path: PathBuf,
    value: Mutex<PersistedAppState>,
    recovery_error: Mutex<Option<String>>,
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
                explorer_surface: ExplorerSurface::Git,
                explorer_collapsed: true,
                agent_sidebar_collapsed: true,
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
