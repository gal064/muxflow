//! The identity an action or dialog is bound to. Port of
//! `features/shell/hostScope.ts`.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostScopeToken {
    pub host_profile_id: String,
    pub connection_key: String,
    pub connection_epoch: u64,
    pub server_identity: Option<String>,
    pub generation: u64,
}

pub fn same_host_scope(left: &HostScopeToken, right: &HostScopeToken) -> bool {
    left == right
}

/// Dialogs may stay open while a topology event advances the generation. A
/// profile switch, reconnect, helper or server restart still invalidates them.
pub fn same_host_connection(left: &HostScopeToken, right: &HostScopeToken) -> bool {
    left.host_profile_id == right.host_profile_id
        && left.connection_key == right.connection_key
        && left.connection_epoch == right.connection_epoch
        && left.server_identity == right.server_identity
}

/// A first helper install begins before the bridge can report a server
/// identity, so that one missing fact may become known. Every durable fact that
/// existed at consent time still has to match.
pub fn same_helper_install_connection(left: &HostScopeToken, right: &HostScopeToken) -> bool {
    left.host_profile_id == right.host_profile_id
        && left.connection_key == right.connection_key
        && left.connection_epoch == right.connection_epoch
        && (left.server_identity.is_none() || left.server_identity == right.server_identity)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token() -> HostScopeToken {
        HostScopeToken {
            host_profile_id: "local".into(),
            connection_key: "local".into(),
            connection_epoch: 1,
            server_identity: Some("server".into()),
            generation: 2,
        }
    }

    fn changed() -> Vec<HostScopeToken> {
        vec![
            HostScopeToken {
                host_profile_id: "other".into(),
                ..token()
            },
            HostScopeToken {
                connection_key: "ssh:other".into(),
                ..token()
            },
            HostScopeToken {
                connection_epoch: 2,
                ..token()
            },
            HostScopeToken {
                server_identity: Some("other".into()),
                ..token()
            },
        ]
    }

    #[test]
    fn invalidates_every_deferred_operation_dimension() {
        assert!(same_host_scope(&token(), &token()));
        for other in changed() {
            assert!(!same_host_scope(&token(), &other));
        }
        let newer = HostScopeToken {
            generation: 3,
            ..token()
        };
        assert!(!same_host_scope(&token(), &newer));
    }

    #[test]
    fn keeps_a_dialog_valid_across_generations_on_the_same_connection() {
        let newer = HostScopeToken {
            generation: 3,
            ..token()
        };
        assert!(same_host_connection(&token(), &newer));
    }

    #[test]
    fn invalidates_a_dialog_when_the_durable_connection_identity_changes() {
        for other in changed() {
            assert!(!same_host_connection(&token(), &other));
        }
    }

    #[test]
    fn lets_a_first_helper_install_establish_only_its_absent_server_identity() {
        let before = HostScopeToken {
            server_identity: None,
            ..token()
        };
        assert!(same_helper_install_connection(&before, &token()));
        let other_server = HostScopeToken {
            server_identity: Some("other".into()),
            ..token()
        };
        assert!(!same_helper_install_connection(&token(), &other_server));
        let other_epoch = HostScopeToken {
            connection_epoch: 2,
            ..token()
        };
        assert!(!same_helper_install_connection(&before, &other_epoch));
        let other_key = HostScopeToken {
            connection_key: "ssh:other".into(),
            ..token()
        };
        assert!(!same_helper_install_connection(&before, &other_key));
    }
}
