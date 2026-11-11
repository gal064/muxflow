//! What the Ports panel decides without a window: port parsing, the host the
//! panel is on, which detected ports are worth suggesting, and which hosts'
//! forwards have lost their host. Port of `features/ports/api.ts`'s
//! `parsePort`, the pure parts of `PortsPanel.tsx` and `usePortForwards.ts`.

use desktop_core::connection::{
    ConnectionSpec,
    ports::{DetectedPort, PortForward},
};

/// What a refused port field says.
pub const BAD_PORTS: &str = "Ports are numbers from 1 to 65535.";

/// A TCP port typed by a person: digits only, 1 to 65535.
pub fn parse_port(text: &str) -> Option<u16> {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.len() > 5 || !trimmed.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    trimmed.parse::<u16>().ok().filter(|port| *port >= 1)
}

/// The two fields' ports: the local one is the remote one unless the person
/// says otherwise.
pub fn forward_ports(remote: &str, local: &str) -> Result<(u16, u16), &'static str> {
    let remote = parse_port(remote);
    let local = if local.trim().is_empty() {
        remote
    } else {
        parse_port(local)
    };
    remote.zip(local).ok_or(BAD_PORTS)
}

/// One shown SSH host. Local has nothing to forward.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortsHost {
    pub profile_id: String,
    pub label: String,
    pub connection: ConnectionSpec,
}

/// The host the panel is on: the one chosen, else the active one, else the
/// first.
pub fn chosen_host<'a>(
    hosts: &'a [PortsHost],
    chosen: Option<&str>,
    active: &str,
) -> Option<&'a PortsHost> {
    let named = |id: &str| hosts.iter().find(|host| host.profile_id == id);
    chosen
        .and_then(named)
        .or_else(|| named(active))
        .or_else(|| hosts.first())
}

/// A detection and the host it was made on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Detection {
    pub profile_id: String,
    pub ports: Vec<DetectedPort>,
}

/// The detected ports not already forwarded from `profile_id`.
pub fn suggestions(
    forwards: &[PortForward],
    profile_id: &str,
    detection: Option<&Detection>,
) -> Vec<DetectedPort> {
    let Some(detection) = detection.filter(|detection| detection.profile_id == profile_id) else {
        return Vec::new();
    };
    detection
        .ports
        .iter()
        .filter(|detected| {
            !forwards.iter().any(|forward| {
                forward.profile_id == profile_id && forward.remote_port == detected.port
            })
        })
        .cloned()
        .collect()
}

/// The hosts whose forwards have lost their host, once the profiles are known:
/// until then no host is shown, and every forward would look orphaned.
pub fn orphaned_hosts(
    forwards: &[PortForward],
    shown: &[String],
    profiles_loaded: bool,
) -> Vec<String> {
    let mut orphaned: Vec<String> = Vec::new();
    if !profiles_loaded {
        return orphaned;
    }
    for forward in forwards {
        if !shown.contains(&forward.profile_id) && !orphaned.contains(&forward.profile_id) {
            orphaned.push(forward.profile_id.clone());
        }
    }
    orphaned
}

/// Where a forward answers on this machine.
pub fn local_url(local_port: u16) -> String {
    format!("http://127.0.0.1:{local_port}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use desktop_core::connection::ports::ForwardState;

    fn host(id: &str) -> PortsHost {
        PortsHost {
            profile_id: id.into(),
            label: id.to_uppercase(),
            connection: ConnectionSpec::Ssh {
                profile_id: id.into(),
                target: id.into(),
                config_path: None,
            },
        }
    }

    fn forward(profile: &str, port: u16) -> PortForward {
        PortForward {
            profile_id: profile.into(),
            remote_port: port,
            local_port: port,
            state: ForwardState::Active,
            error: None,
        }
    }

    fn detected(port: u16, process: Option<&str>) -> DetectedPort {
        DetectedPort {
            port,
            process: process.map(Into::into),
        }
    }

    #[test]
    fn a_port_is_digits_from_one_to_65535() {
        assert_eq!(parse_port("3000"), Some(3000));
        assert_eq!(parse_port(" 80 "), Some(80));
        assert_eq!(parse_port("65535"), Some(65535));
        for bad in [
            "", " ", "0", "65536", "70000", "123456", "-1", "8o", "1.5", "+80", "٣٠٠٠",
        ] {
            assert_eq!(parse_port(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_local_port_is_the_remote_one_unless_told_otherwise() {
        assert_eq!(forward_ports("3000", ""), Ok((3000, 3000)));
        assert_eq!(forward_ports("3000", "  "), Ok((3000, 3000)));
        assert_eq!(forward_ports("5173", "8080"), Ok((5173, 8080)));
        assert_eq!(forward_ports("70000", ""), Err(BAD_PORTS));
        assert_eq!(forward_ports("3000", "x"), Err(BAD_PORTS));
        assert_eq!(forward_ports("", ""), Err(BAD_PORTS));
    }

    #[test]
    fn starts_on_the_active_host_unless_one_was_chosen() {
        let hosts = [host("devbox"), host("gpu")];
        assert_eq!(chosen_host(&hosts, None, "gpu").unwrap().profile_id, "gpu");
        assert_eq!(
            chosen_host(&hosts, Some("devbox"), "gpu")
                .unwrap()
                .profile_id,
            "devbox"
        );
        // A choice that is no longer shown, or an active host that is Local,
        // falls back rather than showing nothing.
        assert_eq!(
            chosen_host(&hosts, Some("gone"), "gpu").unwrap().profile_id,
            "gpu"
        );
        assert_eq!(
            chosen_host(&hosts, None, "local").unwrap().profile_id,
            "devbox"
        );
        assert_eq!(chosen_host(&[], None, "local"), None);
    }

    #[test]
    fn suggests_only_what_this_host_has_not_forwarded() {
        let detection = Detection {
            profile_id: "gpu".into(),
            ports: vec![detected(3000, Some("node")), detected(8000, None)],
        };
        let forwards = [forward("gpu", 3000), forward("devbox", 8000)];
        assert_eq!(
            suggestions(&forwards, "gpu", Some(&detection)),
            [detected(8000, None)]
        );
        // A detection made on another host says nothing about this one.
        assert!(suggestions(&forwards, "devbox", Some(&detection)).is_empty());
        assert!(suggestions(&forwards, "gpu", None).is_empty());
    }

    #[test]
    fn a_host_no_longer_shown_loses_its_forwards_once_profiles_are_loaded() {
        let forwards = [
            forward("devbox", 3000),
            forward("gpu", 8000),
            forward("gpu", 9000),
        ];
        // A native side that outlived a reload must not be cut before any host is shown.
        assert!(orphaned_hosts(&forwards, &[], false).is_empty());
        let both = ["devbox".to_owned(), "gpu".to_owned()];
        assert!(orphaned_hosts(&forwards, &both, true).is_empty());
        let fewer = ["devbox".to_owned()];
        assert_eq!(orphaned_hosts(&forwards, &fewer, true), ["gpu"]);
    }

    #[test]
    fn a_forward_is_opened_on_this_machine() {
        assert_eq!(local_url(8080), "http://127.0.0.1:8080");
    }
}
