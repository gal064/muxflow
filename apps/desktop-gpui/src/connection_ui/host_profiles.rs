//! Host identity helpers. Port of `features/shell/hostProfiles.ts`.

use desktop_core::connection::{ConnectionSpec, profiles::HostProfile};

pub const LOCAL_PROFILE_ID: &str = "local";

/// `hostProfileId` from `features/shell/types.ts`.
pub fn host_profile_id(connection: &ConnectionSpec) -> String {
    match connection {
        ConnectionSpec::Local => LOCAL_PROFILE_ID.to_owned(),
        ConnectionSpec::Ssh { profile_id, .. } => profile_id.clone(),
    }
}

/// `helperConnectionKey` from `features/shell/helperUpgrade.ts`.
pub fn helper_connection_key(connection: &ConnectionSpec) -> String {
    match connection {
        ConnectionSpec::Local => LOCAL_PROFILE_ID.to_owned(),
        ConnectionSpec::Ssh {
            profile_id,
            target,
            config_path,
        } => format!(
            "{profile_id}\0{target}\0{}",
            config_path.as_deref().unwrap_or("")
        ),
    }
}

/// The host mark: its letter, else its label's first character, else its id's.
pub fn host_letter(profile: &HostProfile) -> String {
    let letter = profile
        .letter
        .as_deref()
        .and_then(|value| value.chars().next());
    let from_label = profile.label.trim().chars().next();
    let from_id = profile.id.chars().next();
    letter
        .or(from_label)
        .or(from_id)
        .map(|value| value.to_uppercase().collect())
        .unwrap_or_default()
}

/// FNV-1a over `target\0configPath`, as `savedSshProfileId` computes it, so a
/// host saved by either app keeps its id.
pub fn saved_ssh_profile_id(target: &str, config_path: &str) -> String {
    let slug: String = target
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                character
            } else {
                '-'
            }
        })
        .collect();
    let mut collapsed = String::with_capacity(slug.len());
    for character in slug.chars() {
        if character != '-' || !collapsed.ends_with('-') {
            collapsed.push(character);
        }
    }
    collapsed.truncate(64);
    if collapsed.is_empty() {
        collapsed.push_str("host");
    }
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in format!("{target}\0{config_path}").bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("ssh-{collapsed}-{hash:016x}")
}

pub fn profile_id_for_ssh_connection(
    profiles: &[HostProfile],
    target: &str,
    config_path: &str,
) -> String {
    profiles
        .iter()
        .find(|profile| match &profile.connection {
            ConnectionSpec::Ssh {
                target: saved_target,
                config_path: saved_config,
                ..
            } => saved_target == target && saved_config.as_deref().unwrap_or("") == config_path,
            ConnectionSpec::Local => false,
        })
        .map(|profile| profile.id.clone())
        .unwrap_or_else(|| saved_ssh_profile_id(target, config_path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(id: &str, label: &str, letter: Option<&str>) -> HostProfile {
        HostProfile {
            id: id.into(),
            label: label.into(),
            connection: ConnectionSpec::Local,
            letter: letter.map(str::to_owned),
            shown: false,
        }
    }

    #[test]
    fn host_letter_prefers_the_saved_letter_then_the_label_then_the_id() {
        assert_eq!(host_letter(&profile("ssh-work", "workbox", Some("w"))), "W");
        assert_eq!(host_letter(&profile("local", "Local", None)), "L");
        assert_eq!(
            host_letter(&profile("ssh-work", "  prod.example ", None)),
            "P"
        );
        assert_eq!(host_letter(&profile("ssh-work", "   ", None)), "S");
        assert_eq!(host_letter(&profile("", "", None)), "");
    }

    #[test]
    fn host_letter_takes_one_whole_character() {
        assert_eq!(host_letter(&profile("x", "🦀 crab", None)), "🦀");
        assert_eq!(host_letter(&profile("x", "élan", None)), "É");
    }

    #[test]
    fn saved_ssh_ids_do_not_collide_for_targets_sharing_a_slug() {
        assert_ne!(
            saved_ssh_profile_id("prod.example", ""),
            saved_ssh_profile_id("prod-example", "")
        );
        assert_ne!(
            saved_ssh_profile_id("user@host", "/a"),
            saved_ssh_profile_id("user@host", "/b")
        );
    }

    #[test]
    fn saved_ssh_ids_are_the_ones_the_desktop_app_computed() {
        // Values from the TypeScript `savedSshProfileId`.
        assert_eq!(
            saved_ssh_profile_id("prod.example", ""),
            "ssh-prod-example-51e9d5c03da28b50"
        );
        assert_eq!(
            saved_ssh_profile_id("user@host", "/a"),
            "ssh-user-host-ce5ceef3406bce94"
        );
    }

    #[test]
    fn reuses_a_persisted_id_for_the_exact_same_ssh_connection() {
        let legacy = HostProfile {
            id: "legacy".into(),
            label: "Prod".into(),
            connection: ConnectionSpec::Ssh {
                profile_id: "legacy".into(),
                target: "prod".into(),
                config_path: Some("/cfg".into()),
            },
            letter: None,
            shown: false,
        };
        assert_eq!(
            profile_id_for_ssh_connection(std::slice::from_ref(&legacy), "prod", "/cfg"),
            "legacy"
        );
    }
}
