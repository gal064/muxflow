//! The one line this app owns in Codex's own `config.toml`.
//!
//! Codex 0.157 made `codex` connect to a shared background server by default,
//! and that server runs every hook with its own environment rather than the
//! window's, so a hook cannot say which tmux pane it came from. The tmux
//! server's global environment carries the actual fix (see
//! `tmux_config::apply_codex_embedded_env`): with it every new pane runs Codex
//! without the server. Codex then warns on every launch that it is running
//! without the server — unless `[features] daemon_auto_start` is off, which is
//! this line. It is secondary, not the fix: nothing breaks without it. It is
//! host-wide, so Codex started outside tmux stops starting the server on its
//! own too; a server that is already running is still used there.
//!
//! Ownership is the marker comment above the key, because TOML has nothing
//! like the owner and version a hook command carries. A value the user wrote,
//! `true` or `false`, is theirs and is never changed or removed.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use toml_edit::{DocumentMut, Item, Table, value};

use super::hooks::{
    ConfigLock, backup_path, inspect_config_path, read_config, remove_atomic, write_atomic,
    write_backup_once,
};

const FEATURES: &str = "features";
const DAEMON_AUTO_START: &str = "daemon_auto_start";
const MARKER: &str = "# Added by Muxflow so Codex hooks run in their tmux pane; removed when Muxflow's hooks are uninstalled.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    Unchanged,
    Written,
}

fn config_path(home: &Path) -> PathBuf {
    home.join(".codex").join("config.toml")
}

/// Add the owned `daemon_auto_start = false` when the key is missing.
///
/// Runs on every connect, so it writes nothing when the key is already there,
/// and puts it back when the user deleted it. A host with no `~/.codex` has no
/// Codex, and is not given a configuration directory for a vendor it lacks.
pub(crate) fn ensure(home: &Path) -> anyhow::Result<Outcome> {
    let path = config_path(home);
    if !path.parent().is_some_and(Path::is_dir) {
        return Ok(Outcome::Unchanged);
    }
    rewrite(&path, with_owned_setting)
}

/// Take the owned line back out, and only that line.
pub(crate) fn remove(home: &Path) -> anyhow::Result<Outcome> {
    let path = config_path(home);
    if !path.exists() {
        return Ok(Outcome::Unchanged);
    }
    rewrite(&path, without_owned_setting)
}

/// The hook installer's own safety rules: no symlinks, a lock, a one-time
/// backup, and a compare-and-swap against the bytes the edit was computed from,
/// so a Codex write racing this one is never clobbered.
fn rewrite(
    path: &Path,
    edit: fn(&[u8]) -> anyhow::Result<Option<Vec<u8>>>,
) -> anyhow::Result<Outcome> {
    inspect_config_path(path)?;
    if edit(&read_config(path)?)?.is_none() {
        return Ok(Outcome::Unchanged);
    }
    let _lock = ConfigLock::acquire(path)?;
    let bytes = read_config(path)?;
    let Some(updated) = edit(&bytes)? else {
        return Ok(Outcome::Unchanged);
    };
    if updated.is_empty() && !backup_path(path).exists() {
        // The file did not exist before this app created it.
        remove_atomic(path, &bytes)?;
    } else {
        write_backup_once(path, &bytes)?;
        write_atomic(path, &updated, &bytes)?;
    }
    Ok(Outcome::Written)
}

fn parse(bytes: &[u8]) -> anyhow::Result<DocumentMut> {
    std::str::from_utf8(bytes)
        .context("Codex config.toml is not UTF-8")?
        .parse()
        .context("parse Codex config.toml")
}

fn with_owned_setting(bytes: &[u8]) -> anyhow::Result<Option<Vec<u8>>> {
    let mut document = parse(bytes)?;
    let features = document
        .entry(FEATURES)
        .or_insert_with(|| Item::Table(Table::new()));
    let Some(features) = features.as_table_mut() else {
        // An inline table cannot carry the marker comment, so the key could
        // never be recognised as this app's again.
        bail!("Codex config.toml has a [features] value that is not a table");
    };
    if features.contains_key(DAEMON_AUTO_START) {
        return Ok(None);
    }
    features.insert(DAEMON_AUTO_START, value(false));
    features
        .key_mut(DAEMON_AUTO_START)
        .expect("just inserted")
        .leaf_decor_mut()
        .set_prefix(format!("{MARKER}\n"));
    Ok(Some(document.to_string().into_bytes()))
}

fn without_owned_setting(bytes: &[u8]) -> anyhow::Result<Option<Vec<u8>>> {
    let mut document = parse(bytes)?;
    let Some(features) = document.get_mut(FEATURES).and_then(Item::as_table_mut) else {
        return Ok(None);
    };
    let owned = features
        .key(DAEMON_AUTO_START)
        .and_then(|key| key.leaf_decor().prefix())
        .and_then(|prefix| prefix.as_str())
        .is_some_and(|prefix| prefix.contains(MARKER))
        && features.get(DAEMON_AUTO_START).and_then(Item::as_bool) == Some(false);
    if !owned {
        return Ok(None);
    }
    features.remove(DAEMON_AUTO_START);
    if features.is_empty() {
        document.remove(FEATURES);
    }
    Ok(Some(document.to_string().into_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shaped like a real host's file: Codex writes its hook trust hashes here,
    /// and a changed byte in one of them makes Codex ask to trust the hook again.
    const REAL: &str = r#"[tui]
screen_reader_detection_done = true

[projects."/home/operator"]
trust_level = "trusted" # the user's own comment

[hooks.state."/home/operator/.codex/hooks.json:pre_tool_use:0:0"]
trusted_hash = "sha256:09994fb8b5d193e7a0594c1cf3b119e8e7f9557250d25237789f2e3958f1ffd7"
"#;

    fn added(before: &str) -> String {
        String::from_utf8(with_owned_setting(before.as_bytes()).unwrap().unwrap()).unwrap()
    }

    #[test]
    fn adding_the_setting_leaves_every_existing_byte_in_place() {
        let after = added(REAL);
        assert!(after.starts_with(REAL), "{after}");
        assert!(
            after.ends_with(&format!(
                "[features]\n{MARKER}\ndaemon_auto_start = false\n"
            )),
            "{after}"
        );
        let document: DocumentMut = after.parse().unwrap();
        assert_eq!(document[FEATURES][DAEMON_AUTO_START].as_bool(), Some(false));
    }

    #[test]
    fn an_existing_features_table_gains_only_the_key() {
        let before = "[features]\nweb_search = true\n\n[tui]\nx = 1\n";
        let after = added(before);
        assert_eq!(
            after,
            format!(
                "[features]\nweb_search = true\n{MARKER}\ndaemon_auto_start = false\n\n[tui]\nx = 1\n"
            )
        );
    }

    #[test]
    fn a_value_the_user_set_is_theirs_either_way() {
        for before in [
            "[features]\ndaemon_auto_start = true\n",
            "[features]\ndaemon_auto_start = false\n",
            "features.daemon_auto_start = true\n",
        ] {
            assert_eq!(
                with_owned_setting(before.as_bytes()).unwrap(),
                None,
                "{before}"
            );
            assert_eq!(
                without_owned_setting(before.as_bytes()).unwrap(),
                None,
                "{before}"
            );
        }
    }

    #[test]
    fn an_added_setting_is_not_added_twice() {
        let after = added(REAL);
        assert_eq!(with_owned_setting(after.as_bytes()).unwrap(), None);
    }

    #[test]
    fn removal_restores_the_original_bytes() {
        for before in [REAL, "", "[features]\nweb_search = true\n"] {
            let after = added(before);
            let restored = without_owned_setting(after.as_bytes()).unwrap().unwrap();
            assert_eq!(String::from_utf8(restored).unwrap(), before);
        }
    }

    #[test]
    fn a_setting_the_user_changed_after_it_was_added_is_kept() {
        let after = added(REAL).replace("daemon_auto_start = false", "daemon_auto_start = true");
        assert_eq!(without_owned_setting(after.as_bytes()).unwrap(), None);
    }

    #[test]
    fn a_file_codex_itself_could_not_read_is_left_alone() {
        assert!(with_owned_setting(b"[features\n").is_err());
        assert!(with_owned_setting(b"features = { web_search = true }\n").is_err());
    }

    #[test]
    fn ensure_writes_once_and_remove_takes_back_only_its_own_line() {
        let home = tempfile::tempdir().unwrap();
        // No Codex on this host: nothing is created.
        assert_eq!(ensure(home.path()).unwrap(), Outcome::Unchanged);
        assert!(!home.path().join(".codex").exists());

        std::fs::create_dir(home.path().join(".codex")).unwrap();
        let path = config_path(home.path());
        std::fs::write(&path, REAL).unwrap();
        assert_eq!(ensure(home.path()).unwrap(), Outcome::Written);
        assert_eq!(ensure(home.path()).unwrap(), Outcome::Unchanged);
        assert_eq!(std::fs::read_to_string(backup_path(&path)).unwrap(), REAL);
        assert_eq!(remove(home.path()).unwrap(), Outcome::Written);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), REAL);
        assert_eq!(remove(home.path()).unwrap(), Outcome::Unchanged);
    }

    #[test]
    fn a_config_this_app_created_is_removed_again() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join(".codex")).unwrap();
        assert_eq!(ensure(home.path()).unwrap(), Outcome::Written);
        let path = config_path(home.path());
        assert!(path.exists());
        assert_eq!(remove(home.path()).unwrap(), Outcome::Written);
        assert!(!path.exists());
    }

    #[test]
    fn a_symlinked_config_is_never_written_through() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join(".codex")).unwrap();
        let target = home.path().join("dotfiles.toml");
        std::fs::write(&target, REAL).unwrap();
        std::os::unix::fs::symlink(&target, config_path(home.path())).unwrap();
        assert!(ensure(home.path()).is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), REAL);
    }
}
