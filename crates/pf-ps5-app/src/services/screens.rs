//! The host's screens for this device: `GET /api/v1/display/targets` and the pick's `PUT
//! /api/v1/display/clients/{this device's fingerprint}`, on the library's mTLS lane and pin.
//! The console words the rows (`pf_client_core::display_targets`); this only moves them.
use std::time::Duration;

use pf_client_core::display_targets::{Pick, Targets};

use crate::services::budget;
use crate::services::library::{agent_within, base_url, classify, get_json, LibraryError};

/// The floor between two asks of one host: each is a TLS handshake, and `in_use_by` moves only
/// as devices start and end streams. The desktop's TTL.
pub const REFRESH_FLOOR: Duration = Duration::from_secs(20);

/// The host's answer as it sent it. `None` on any miss: a host without the route has no Screen
/// section, which is not an error.
pub fn fetch(addr: &str, mgmt_port: u16, identity: &(String, String), pin: Option<[u8; 32]>) -> Option<Targets> {
    let path = "/api/v1/display/targets";
    get_json::<Targets>(addr, mgmt_port, identity, pin, path, budget::REQUEST)
        .inspect_err(|e| tracing::debug!("display targets ({addr}:{mgmt_port}): {e:?}"))
        .ok()
}

/// Writes this device's pick, blocking. Any answer outside 2xx is [`LibraryError::Http`], a 403
/// included: the host refuses a device without the screen grant that way.
pub fn set_pick(
    addr: &str,
    mgmt_port: u16,
    identity: &(String, String),
    pin: Option<[u8; 32]>,
    pick: &Pick,
) -> Result<(), LibraryError> {
    let device = punktfunk_core::quic::endpoint::fingerprint_of_pem(&identity.0)
        .map_err(|e| LibraryError::Identity(e.to_string()))?;
    let url = format!(
        "{}/api/v1/display/clients/{}",
        base_url(addr, mgmt_port),
        crate::services::store::shared::hex(device)
    );
    let agent = agent_within(identity, pin, budget::REQUEST)?;
    match agent
        .put(url.as_str())
        .header("content-type", "application/json")
        .send(body(pick))
    {
        Ok(_) => Ok(()),
        Err(ureq::Error::StatusCode(code)) => Err(LibraryError::Http(code)),
        Err(e) => Err(classify(e)),
    }
}

/// The PUT body: the screen, and a role's topology beside it.
fn body(pick: &Pick) -> String {
    serde_json::to_string(pick).expect("serialize a screen pick")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pf_client_core::display_targets::{DisplayTarget, Role, RoleInfo};

    /// The answer reaches the console as the host sent it, `may_choose` and `role` included.
    #[test]
    fn the_answer_keeps_the_grant_and_the_roles() {
        let answer = r#"{ "locked": false, "may_choose": false, "host_default": "own", "yours": null,
            "targets": [ { "id": "own", "label": "Own screen" },
                         { "id": "HDMI-A-1", "label": "LG TV", "connector": "HDMI-A-1" } ],
            "role": { "yours": "second", "host_default": "only", "offered": ["only", "second", "spare"] } }"#;
        let t: Targets = serde_json::from_str(answer).expect("the answer decodes");
        assert!(!t.may_choose);
        assert_eq!(t.targets.len(), 2);
        assert_eq!(
            t.role,
            Some(RoleInfo {
                yours: Some(Role::Second),
                host_default: Role::Only,
                offered: vec![Role::Only, Role::Second],
            })
        );
        let older: Targets = serde_json::from_str(r#"{ "host_default": "own", "targets": [] }"#).unwrap();
        assert!(
            older.may_choose && older.role.is_none(),
            "a host before the grant lets every device"
        );
    }

    /// A role carries its topology; a monitor sends the screen alone.
    #[test]
    fn the_pick_body_is_the_pair() {
        let role = Pick {
            screen: DisplayTarget::Own,
            topology: Some("extend".into()),
        };
        assert_eq!(body(&role), r#"{"screen":"own","topology":"extend"}"#);
        let monitor = Pick {
            screen: DisplayTarget::Monitor("HDMI-A-1".into()),
            topology: None,
        };
        assert_eq!(body(&monitor), r#"{"screen":{"monitor":"HDMI-A-1"}}"#);
    }
}
