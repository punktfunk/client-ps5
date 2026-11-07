//! User-facing sentences for `punktfunk-core`'s error and rejection types.
//!
//! A typed rejection is worded by `pf-client-core`; the rest read the client and console
//! messages. Without this, failures render as Debug strings (e.g. "connect: Rejected(Busy)").
use pf_client_core::trust::connect_reject_message;
use pf_l10n::{client, console};
use punktfunk_core::PunktfunkError;

/// Why connect/probe failed (distinguishes rejection from transport trouble).
fn connect_message(err: &PunktfunkError) -> String {
    match err {
        PunktfunkError::Rejected(reason, _) => connect_reject_message(*reason),
        PunktfunkError::Timeout => client::error_host_timeout().into(),
        PunktfunkError::Io(e) => {
            tracing::warn!(error = %e, "host unreachable");
            client::error_host_unreachable().into()
        }
        PunktfunkError::Closed => console::console_error_host_closed().into(),
        other => console::console_error_connect_failed_detail(other),
    }
}

/// Why PIN pairing failed (Crypto = wrong PIN, not network problem).
pub fn pair_message(err: &PunktfunkError) -> String {
    match err {
        PunktfunkError::Crypto => client::pair_wrong_pin().into(),
        other => connect_message(other),
    }
}

/// Why a request for access failed. It has no PIN, so Crypto is a host that didn't match its advert.
pub fn access_message(err: &PunktfunkError) -> String {
    match err {
        PunktfunkError::Crypto => client::pair_identity_mismatch().into(),
        other => connect_message(other),
    }
}

/// Extract `PunktfunkError` from anyhow chain for user-facing messages.
pub fn friendly(err: &anyhow::Error) -> String {
    err.downcast_ref::<PunktfunkError>()
        .map_or_else(|| format!("{err:#}"), connect_message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use punktfunk_core::reject::RejectReason;

    #[test]
    fn a_refusal_reads_the_cores_sentence() {
        for &r in RejectReason::ALL {
            let err = PunktfunkError::Rejected(r, String::new());
            assert_eq!(connect_message(&err), connect_reject_message(r), "{r:?}");
        }
    }

    #[test]
    fn transport_trouble_reads_the_client_messages() {
        assert_eq!(connect_message(&PunktfunkError::Timeout), client::error_host_timeout());
        assert_eq!(
            connect_message(&PunktfunkError::Closed),
            console::console_error_host_closed()
        );
        assert_eq!(pair_message(&PunktfunkError::Crypto), client::pair_wrong_pin());
        assert_eq!(
            access_message(&PunktfunkError::Crypto),
            client::pair_identity_mismatch()
        );
    }
}
