use std::{collections::HashSet, fs, os::unix::fs::PermissionsExt, path::PathBuf, sync::Mutex};

use serde::{Deserialize, Serialize};
use tauri::State;
use uuid::Uuid;

use super::{ConnectionSpec, validate_profile_id};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HostProfile {
    pub id: String,
    pub label: String,
    pub connection: ConnectionSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedProfiles {
    schema_version: u32,
    profiles: Vec<HostProfile>,
    last_profile_id: Option<String>,
}

impl Default for PersistedProfiles {
    fn default() -> Self {
        Self {
            schema_version: 1,
            profiles: vec![HostProfile {
                id: "local".into(),
                label: "Local".into(),
                connection: ConnectionSpec::Local,
            }],
            last_profile_id: Some("local".into()),
        }
    }
}

pub struct ProfileStore {
    path: PathBuf,
    value: Mutex<PersistedProfiles>,
    recovery: Mutex<Option<ProfileRecovery>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProfileRecovery {
    preserved_path: String,
    error: String,
}

impl ProfileStore {
    pub fn load(path: PathBuf) -> Result<Self, String> {
        let mut recovery = None;
        let mut value = if path.exists() {
            let bytes = fs::read(&path).map_err(|error| error.to_string())?;
            match serde_json::from_slice::<PersistedProfiles>(&bytes).and_then(|mut parsed| {
                if parsed.schema_version != 1 {
                    return Err(serde::de::Error::custom(format!(
                        "unsupported profile schema {}",
                        parsed.schema_version
                    )));
                }
                normalize_and_validate(&mut parsed).map_err(serde::de::Error::custom)?;
                Ok(parsed)
            }) {
                Ok(parsed) => parsed,
                Err(error) => {
                    let preserved = path.with_extension(format!("corrupt-{}.json", Uuid::new_v4()));
                    fs::rename(&path, &preserved).map_err(|rename| {
                        format!("profile recovery failed after {error}: {rename}")
                    })?;
                    recovery = Some(ProfileRecovery {
                        preserved_path: preserved.to_string_lossy().into_owned(),
                        error: error.to_string(),
                    });
                    PersistedProfiles::default()
                }
            }
        } else {
            PersistedProfiles::default()
        };
        normalize_and_validate(&mut value)?;
        Ok(Self {
            path,
            value: Mutex::new(value),
            recovery: Mutex::new(recovery),
        })
    }

    #[cfg(test)]
    fn save(&self) -> Result<(), String> {
        let value = self.value.lock().unwrap().clone();
        self.persist(&value)
    }

    fn persist(&self, value: &PersistedProfiles) -> Result<(), String> {
        let parent = self.path.parent().ok_or("profile path has no parent")?;
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
        let data = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
        let temporary = self
            .path
            .with_extension(format!("{}.partial", Uuid::new_v4()));
        let result: Result<(), String> = (|| {
            fs::write(&temporary, data).map_err(|error| error.to_string())?;
            fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))
                .map_err(|error| error.to_string())?;
            fs::rename(&temporary, &self.path).map_err(|error| error.to_string())?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    pub(crate) fn connection_for(&self, profile_id: &str) -> Result<ConnectionSpec, String> {
        self.value
            .lock()
            .unwrap()
            .profiles
            .iter()
            .find(|profile| profile.id == profile_id)
            .map(|profile| profile.connection.clone())
            .ok_or_else(|| format!("host profile {profile_id} does not exist"))
    }

    fn reset_transactionally(&self) -> Result<(), String> {
        let next = PersistedProfiles::default();
        self.persist(&next)?;
        *self.value.lock().unwrap() = next;
        *self.recovery.lock().unwrap() = None;
        Ok(())
    }

    fn save_profile_transactionally(&self, profile: HostProfile) -> Result<(), String> {
        let mut next = self.value.lock().unwrap().clone();
        if let Some(existing) = next.profiles.iter_mut().find(|item| item.id == profile.id) {
            *existing = profile.clone();
        } else {
            next.profiles.push(profile.clone());
        }
        next.last_profile_id = Some(profile.id);
        normalize_and_validate(&mut next)?;
        self.persist(&next)?;
        *self.value.lock().unwrap() = next;
        Ok(())
    }

    /// Removes a saved host, leaving the file valid whatever it pointed at.
    ///
    /// `normalize_and_validate` refuses an empty list and refuses a
    /// `lastProfileId` naming a profile that is gone, so both are settled here
    /// rather than discovered on the next launch — the corrupt-file recovery
    /// path is for files this app did not write.
    fn delete_profile_transactionally(&self, profile_id: &str) -> Result<(), String> {
        let mut next = self.value.lock().unwrap().clone();
        if !next.profiles.iter().any(|item| item.id == profile_id) {
            return Err(format!("host profile {profile_id} does not exist"));
        }
        if next.profiles.len() == 1 {
            return Err("the last host profile cannot be deleted".into());
        }
        next.profiles.retain(|item| item.id != profile_id);
        if next.last_profile_id.as_deref() == Some(profile_id) {
            next.last_profile_id = next.profiles.first().map(|profile| profile.id.clone());
        }
        normalize_and_validate(&mut next)?;
        self.persist(&next)?;
        *self.value.lock().unwrap() = next;
        Ok(())
    }
}

fn normalize_and_validate(value: &mut PersistedProfiles) -> Result<(), String> {
    if value.profiles.is_empty() {
        return Err("profile list cannot be empty".into());
    }
    let mut ids = HashSet::new();
    for profile in &mut value.profiles {
        validate_profile_id(&profile.id)?;
        if !ids.insert(profile.id.clone()) {
            return Err(format!("duplicate host profile {}", profile.id));
        }
        if profile.label.trim().is_empty() {
            return Err(format!("host profile {} has an empty label", profile.id));
        }
        if let ConnectionSpec::Ssh { profile_id, .. } = &mut profile.connection {
            if profile_id.is_empty() {
                *profile_id = profile.id.clone();
            }
            if profile_id != &profile.id {
                return Err(format!(
                    "SSH connection identity does not match profile {}",
                    profile.id
                ));
            }
        }
        profile.connection.validate()?;
    }
    if let Some(last) = value.last_profile_id.as_deref()
        && !ids.contains(last)
    {
        return Err("last host profile does not exist".into());
    }
    Ok(())
}

#[tauri::command]
pub fn list_host_profiles(store: State<'_, ProfileStore>) -> Result<serde_json::Value, String> {
    let value = store.value.lock().unwrap().clone();
    let recovery = store.recovery.lock().unwrap().clone();
    Ok(serde_json::json!({
        "schemaVersion": value.schema_version,
        "profiles": value.profiles,
        "lastProfileId": value.last_profile_id,
        "recovery": recovery,
    }))
}

#[tauri::command]
pub fn reset_host_profiles(store: State<'_, ProfileStore>) -> Result<(), String> {
    store.reset_transactionally()
}

#[tauri::command]
pub fn save_host_profile(
    profile: HostProfile,
    store: State<'_, ProfileStore>,
) -> Result<(), String> {
    profile.connection.validate()?;
    validate_profile_id(&profile.id)?;
    if profile.label.trim().is_empty() {
        return Err("profile label cannot be empty".into());
    }
    store.save_profile_transactionally(profile)
}

#[tauri::command]
pub fn delete_host_profile(
    profile_id: String,
    store: State<'_, ProfileStore>,
) -> Result<serde_json::Value, String> {
    validate_profile_id(&profile_id)?;
    store.delete_profile_transactionally(&profile_id)?;
    // The surviving list, so the caller re-renders from what was written rather
    // than from its own guess at what a delete did.
    list_host_profiles(store)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persistence_is_atomic_private_and_secret_free() {
        let root = std::env::temp_dir().join(format!("ade-profiles-{}", Uuid::new_v4()));
        let store = ProfileStore::load(root.join("profiles.json")).unwrap();
        store.save().unwrap();
        let text = fs::read_to_string(root.join("profiles.json")).unwrap();
        assert!(text.contains("schemaVersion"));
        assert!(!text.contains("password"));
        assert_eq!(
            fs::metadata(root.join("profiles.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ssh_profile_round_trips_target_and_config_without_credentials() {
        let root = std::env::temp_dir().join(format!("ade-profiles-{}", Uuid::new_v4()));
        let path = root.join("profiles.json");
        let store = ProfileStore::load(path.clone()).unwrap();
        {
            let mut value = store.value.lock().unwrap();
            value.profiles.push(HostProfile {
                id: "ssh-work".into(),
                label: "Work".into(),
                connection: ConnectionSpec::Ssh {
                    profile_id: "ssh-work".into(),
                    target: "workbox".into(),
                    config_path: Some("/tmp/ssh-config".into()),
                },
            });
            value.last_profile_id = Some("ssh-work".into());
        }
        store.save().unwrap();

        let text = fs::read_to_string(&path).unwrap();
        for forbidden in ["password", "privateKey", "passphrase", "secret"] {
            assert!(!text.contains(forbidden));
        }
        let reloaded = ProfileStore::load(path).unwrap();
        let value = reloaded.value.lock().unwrap();
        assert_eq!(value.last_profile_id.as_deref(), Some("ssh-work"));
        assert!(value.profiles.iter().any(|profile| {
            profile.id == "ssh-work"
                && profile.connection
                    == ConnectionSpec::Ssh {
                        profile_id: "ssh-work".into(),
                        target: "workbox".into(),
                        config_path: Some("/tmp/ssh-config".into()),
                    }
        }));
        drop(value);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn deleting_a_saved_host_rewrites_the_file_and_rehomes_the_last_profile() {
        let root = std::env::temp_dir().join(format!("ade-profiles-{}", Uuid::new_v4()));
        let path = root.join("profiles.json");
        let store = ProfileStore::load(path.clone()).unwrap();
        store
            .save_profile_transactionally(HostProfile {
                id: "ssh-omarchy".into(),
                label: "omarchy".into(),
                connection: ConnectionSpec::Ssh {
                    profile_id: "ssh-omarchy".into(),
                    target: "omarchy".into(),
                    config_path: None,
                },
            })
            .unwrap();

        // The deleted profile was also the last one used, so the pointer has to
        // move with it or the next launch reads a file it will call corrupt.
        store.delete_profile_transactionally("ssh-omarchy").unwrap();
        let reloaded = ProfileStore::load(path).unwrap();
        let value = reloaded.value.lock().unwrap();
        assert!(!value.profiles.iter().any(|item| item.id == "ssh-omarchy"));
        assert_eq!(value.last_profile_id.as_deref(), Some("local"));
        drop(value);

        assert!(
            store
                .delete_profile_transactionally("ssh-omarchy")
                .unwrap_err()
                .contains("does not exist")
        );
        // Emptying the list would make the file itself invalid.
        assert!(
            store
                .delete_profile_transactionally("local")
                .unwrap_err()
                .contains("last host profile")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn corrupt_and_future_profiles_are_preserved_and_recover_to_defaults() {
        for contents in [
            "{broken",
            r#"{"schemaVersion":99,"profiles":[],"lastProfileId":null}"#,
            r#"{"schemaVersion":1,"profiles":[{"id":"bad id","label":"","connection":{"mode":"local"}}],"lastProfileId":"missing"}"#,
        ] {
            let root =
                std::env::temp_dir().join(format!("ade-profiles-recovery-{}", Uuid::new_v4()));
            fs::create_dir_all(&root).unwrap();
            let path = root.join("profiles.json");
            fs::write(&path, contents).unwrap();
            let store = ProfileStore::load(path.clone()).unwrap();
            assert_eq!(store.value.lock().unwrap().profiles[0].id, "local");
            let recovery = store.recovery.lock().unwrap().clone().unwrap();
            assert!(PathBuf::from(&recovery.preserved_path).exists());
            assert!(!path.exists());
            store.save().unwrap();
            assert!(path.exists());
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn failed_profile_update_or_reset_does_not_mutate_memory() {
        let root = std::env::temp_dir().join(format!("ade-profiles-txn-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let blocked_path = root.join("profiles.json");
        fs::create_dir(&blocked_path).unwrap();
        let store = ProfileStore {
            path: blocked_path,
            value: Mutex::new(PersistedProfiles::default()),
            recovery: Mutex::new(Some(ProfileRecovery {
                preserved_path: "preserved".into(),
                error: "original".into(),
            })),
        };
        let before = store.value.lock().unwrap().clone();
        assert!(
            store
                .save_profile_transactionally(HostProfile {
                    id: "work".into(),
                    label: "Work".into(),
                    connection: ConnectionSpec::Local,
                })
                .is_err()
        );
        assert_eq!(store.value.lock().unwrap().profiles, before.profiles);
        assert!(store.reset_transactionally().is_err());
        assert_eq!(store.value.lock().unwrap().profiles, before.profiles);
        assert!(store.recovery.lock().unwrap().is_some());
        fs::remove_dir_all(root).unwrap();
    }
}
