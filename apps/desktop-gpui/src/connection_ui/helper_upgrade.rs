//! The remote helper's probe, confirm and install states. Port of
//! `features/shell/helperUpgrade.ts`.

use desktop_core::connection::helper::HelperRollback;
use serde::Deserialize;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteHelperProbe {
    pub operating_system: String,
    pub architecture: String,
    pub tmux_version: String,
    pub git_version: String,
    pub installed: bool,
    #[serde(default)]
    pub helper_version: Option<String>,
    pub compatible: bool,
    #[serde(default)]
    pub digest: Option<String>,
    pub remote_path: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HelperOperation {
    Install,
    Upgrade,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum HelperUpgradeState {
    #[default]
    Idle,
    Probing {
        connection_key: String,
    },
    Ready {
        connection_key: String,
        probe: RemoteHelperProbe,
    },
    Confirming {
        connection_key: String,
        probe: RemoteHelperProbe,
        operation: HelperOperation,
    },
    Upgrading {
        connection_key: String,
        probe: RemoteHelperProbe,
        operation: HelperOperation,
    },
    Succeeded {
        connection_key: String,
        message: String,
        operation: HelperOperation,
    },
    Failed {
        connection_key: String,
        message: String,
        rollback: HelperRollback,
        probe: Option<RemoteHelperProbe>,
    },
}

impl HelperUpgradeState {
    fn connection_key(&self) -> Option<&str> {
        match self {
            Self::Idle => None,
            Self::Probing { connection_key }
            | Self::Ready { connection_key, .. }
            | Self::Confirming { connection_key, .. }
            | Self::Upgrading { connection_key, .. }
            | Self::Succeeded { connection_key, .. }
            | Self::Failed { connection_key, .. } => Some(connection_key),
        }
    }

    /// Host-level setup questions wait while the helper owns the modal lane.
    pub fn owns_host_setup_lane(&self) -> bool {
        matches!(
            self,
            Self::Probing { .. } | Self::Confirming { .. } | Self::Upgrading { .. }
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HelperUpgradeAction {
    Probe {
        connection_key: String,
    },
    AbandonProbe {
        connection_key: String,
    },
    InvalidateConnectionProbe {
        connection_key: String,
    },
    ProbeSucceeded {
        connection_key: String,
        probe: RemoteHelperProbe,
    },
    ProbeFailed {
        connection_key: String,
        message: String,
    },
    RequestUpgrade,
    CancelUpgrade,
    Upgrade,
    UpgradeSucceeded {
        connection_key: String,
        message: String,
    },
    UpgradeFailed {
        connection_key: String,
        message: String,
        rollback: HelperRollback,
    },
    Reset,
}

pub fn helper_upgrade_reducer(state: &mut HelperUpgradeState, action: HelperUpgradeAction) {
    use HelperUpgradeAction as Action;
    use HelperUpgradeState as State;
    let next = match (std::mem::take(state), action) {
        (_, Action::Reset) => State::Idle,
        // Replacing the helper can make the native supervisor reconnect before
        // the install returns. That fresh transport may probe, but it cannot
        // take ownership away from the install whose result still has to land.
        (current @ State::Upgrading { .. }, Action::Probe { .. }) => current,
        (_, Action::Probe { connection_key }) => State::Probing { connection_key },
        (
            State::Probing { connection_key },
            Action::AbandonProbe {
                connection_key: key,
            },
        ) if connection_key == key => State::Idle,
        (
            current @ (State::Upgrading { .. } | State::Succeeded { .. }),
            Action::InvalidateConnectionProbe { .. },
        ) => current,
        (current, Action::InvalidateConnectionProbe { connection_key })
            if current.connection_key() == Some(connection_key.as_str()) =>
        {
            State::Idle
        }
        (
            State::Probing { connection_key },
            Action::ProbeSucceeded {
                connection_key: key,
                probe,
            },
        ) if connection_key == key => State::Ready {
            connection_key,
            probe,
        },
        (
            State::Probing { connection_key },
            Action::ProbeFailed {
                connection_key: key,
                message,
            },
        ) if connection_key == key => State::Failed {
            connection_key,
            message,
            rollback: HelperRollback::NotNeeded,
            probe: None,
        },
        (
            State::Ready {
                connection_key,
                probe,
            },
            Action::RequestUpgrade,
        ) => {
            let operation = if probe.installed {
                HelperOperation::Upgrade
            } else {
                HelperOperation::Install
            };
            State::Confirming {
                connection_key,
                probe,
                operation,
            }
        }
        (
            State::Confirming {
                connection_key,
                probe,
                ..
            },
            Action::CancelUpgrade,
        ) => State::Ready {
            connection_key,
            probe,
        },
        (
            State::Confirming {
                connection_key,
                probe,
                operation,
            },
            Action::Upgrade,
        ) => State::Upgrading {
            connection_key,
            probe,
            operation,
        },
        (
            State::Upgrading {
                connection_key,
                operation,
                ..
            },
            Action::UpgradeSucceeded {
                connection_key: key,
                message,
            },
        ) if connection_key == key => State::Succeeded {
            connection_key,
            message,
            operation,
        },
        (
            State::Upgrading {
                connection_key,
                probe,
                ..
            },
            Action::UpgradeFailed {
                connection_key: key,
                message,
                rollback,
            },
        ) if connection_key == key => State::Failed {
            connection_key,
            message,
            rollback,
            probe: Some(probe),
        },
        (current, _) => current,
    };
    *state = next;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe() -> RemoteHelperProbe {
        RemoteHelperProbe {
            operating_system: "Linux".into(),
            architecture: "x86_64".into(),
            tmux_version: "tmux 3.3a".into(),
            git_version: "git version 2.39".into(),
            installed: true,
            helper_version: Some("0.0.1".into()),
            compatible: false,
            digest: None,
            remote_path: "$HOME/.local/bin/muxflow-host".into(),
        }
    }

    fn step(mut state: HelperUpgradeState, action: HelperUpgradeAction) -> HelperUpgradeState {
        helper_upgrade_reducer(&mut state, action);
        state
    }

    fn start_probe(key: &str) -> HelperUpgradeAction {
        HelperUpgradeAction::Probe {
            connection_key: key.into(),
        }
    }

    fn probed(key: &str) -> HelperUpgradeState {
        step(
            step(HelperUpgradeState::Idle, start_probe(key)),
            HelperUpgradeAction::ProbeSucceeded {
                connection_key: key.into(),
                probe: probe(),
            },
        )
    }

    fn confirming() -> HelperUpgradeState {
        step(probed("host-a"), HelperUpgradeAction::RequestUpgrade)
    }

    #[test]
    fn requires_a_probe_and_explicit_confirmation_before_upgrade() {
        let confirming = confirming();
        assert!(matches!(confirming, HelperUpgradeState::Confirming { .. }));
        assert!(matches!(
            step(confirming, HelperUpgradeAction::Upgrade),
            HelperUpgradeState::Upgrading { .. }
        ));
    }

    #[test]
    fn distinguishes_successful_rollback_from_rollback_failure_and_remains_retryable() {
        let upgrading = step(confirming(), HelperUpgradeAction::Upgrade);
        let restored = step(
            upgrading.clone(),
            HelperUpgradeAction::UpgradeFailed {
                connection_key: "host-a".into(),
                message: "handshake failed".into(),
                rollback: HelperRollback::Restored,
            },
        );
        assert!(matches!(
            &restored,
            HelperUpgradeState::Failed {
                rollback: HelperRollback::Restored,
                probe: Some(_),
                ..
            }
        ));
        let failed = step(
            upgrading,
            HelperUpgradeAction::UpgradeFailed {
                connection_key: "host-a".into(),
                message: "disk full".into(),
                rollback: HelperRollback::Failed,
            },
        );
        assert!(matches!(
            failed,
            HelperUpgradeState::Failed {
                rollback: HelperRollback::Failed,
                ..
            }
        ));
        assert!(matches!(
            step(restored.clone(), start_probe("host-a")),
            HelperUpgradeState::Probing { .. }
        ));
        assert_eq!(
            step(
                restored.clone(),
                HelperUpgradeAction::AbandonProbe {
                    connection_key: "host-a".into()
                }
            ),
            restored
        );
    }

    #[test]
    fn abandons_only_the_probe_for_the_connection_that_actually_dropped() {
        let probing = step(HelperUpgradeState::Idle, start_probe("host-a"));
        assert_eq!(
            step(
                probing.clone(),
                HelperUpgradeAction::AbandonProbe {
                    connection_key: "host-b".into()
                }
            ),
            probing
        );
        assert_eq!(
            step(
                probing,
                HelperUpgradeAction::AbandonProbe {
                    connection_key: "host-a".into()
                }
            ),
            HelperUpgradeState::Idle
        );
    }

    #[test]
    fn invalidates_a_settled_connection_probe_without_interrupting_an_upgrade() {
        let confirming = confirming();
        let upgrading = step(confirming.clone(), HelperUpgradeAction::Upgrade);
        assert_eq!(
            step(
                confirming,
                HelperUpgradeAction::InvalidateConnectionProbe {
                    connection_key: "host-a".into()
                }
            ),
            HelperUpgradeState::Idle
        );
        assert_eq!(
            step(
                upgrading.clone(),
                HelperUpgradeAction::InvalidateConnectionProbe {
                    connection_key: "host-a".into()
                }
            ),
            upgrading
        );
        assert_eq!(step(upgrading.clone(), start_probe("host-a")), upgrading);
    }

    #[test]
    fn discards_stale_helper_completions_after_a_host_change() {
        let host_b = step(
            step(HelperUpgradeState::Idle, start_probe("host-a")),
            start_probe("host-b"),
        );
        assert_eq!(
            step(
                host_b.clone(),
                HelperUpgradeAction::ProbeSucceeded {
                    connection_key: "host-a".into(),
                    probe: probe()
                }
            ),
            host_b
        );
    }

    #[test]
    fn owns_the_host_setup_lane_only_while_checking_confirming_or_replacing() {
        let probing = step(HelperUpgradeState::Idle, start_probe("host-a"));
        let ready = probed("host-a");
        let confirming = confirming();
        let upgrading = step(confirming.clone(), HelperUpgradeAction::Upgrade);
        let failed = step(
            upgrading.clone(),
            HelperUpgradeAction::UpgradeFailed {
                connection_key: "host-a".into(),
                message: "no".into(),
                rollback: HelperRollback::Restored,
            },
        );
        assert!(!HelperUpgradeState::Idle.owns_host_setup_lane());
        assert!(probing.owns_host_setup_lane());
        assert!(confirming.owns_host_setup_lane());
        assert!(upgrading.owns_host_setup_lane());
        assert!(!ready.owns_host_setup_lane());
        assert!(!failed.owns_host_setup_lane());
    }
}
