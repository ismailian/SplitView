//! LAN discovery: advertise the server over mDNS so the phone can find it
//! without typing an address (`docs/remote-control-desktop.md` §8, tier 1).
//!
//! Discovery is a convenience, never a credential. The TXT record carries only
//! what is already on the QR code's public half — host name and a fingerprint
//! prefix the user can eyeball — and never the pairing code. A device that finds
//! us still has to pin the certificate, present a code and be approved.
//!
//! mDNS also cannot cross a subnet, which is exactly why tier 2 (Tailscale or
//! any other overlay) is documented as the real answer for "when I'm not at my
//! laptop": there the phone dials the tailnet address from the QR instead.

use mdns_sd::{ServiceDaemon, ServiceInfo};

use super::identity::host_name;

/// The service type SplitView advertises.
pub const SERVICE_TYPE: &str = "_splitview._tcp.local.";

/// A live mDNS advertisement. Dropping it unregisters the service and shuts the
/// responder down, so disabling remote control also removes us from the network.
pub struct Advert {
    daemon: ServiceDaemon,
    fullname: String,
}

impl Drop for Advert {
    fn drop(&mut self) {
        // Both calls return a receiver for the operation's outcome, which we do
        // not wait on: the app is not going to do anything differently if a
        // goodbye packet is missed.
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
    }
}

/// Advertise the server on the LAN.
///
/// Returns `None` when mDNS is unavailable (no multicast, a firewall, a
/// container) — discovery is optional and its absence must never stop the
/// server from starting. The QR still carries the address.
pub fn advertise(port: u16) -> Option<Advert> {
    let daemon = ServiceDaemon::new().ok()?;
    let host = host_name();
    let instance = sanitize_instance(&host);

    let properties = [("host", host.as_str()), ("v", "1")];
    let info = ServiceInfo::new(
        SERVICE_TYPE,
        &instance,
        &format!("{instance}.local."),
        (),
        port,
        &properties[..],
    )
    .ok()?
    // Let the responder fill in this machine's addresses rather than guessing
    // which interface the phone is on.
    .enable_addr_auto();

    let fullname = info.get_fullname().to_string();
    daemon.register(info).ok()?;
    Some(Advert { daemon, fullname })
}

/// An mDNS instance name is a DNS label: no dots, no spaces, nothing exotic.
/// A machine called "Ismail's MacBook Pro" must still resolve to something.
fn sanitize_instance(host: &str) -> String {
    let cleaned: String = host
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('-');
    if trimmed.is_empty() {
        "splitview".to_string()
    } else {
        // DNS labels cap at 63 octets.
        trimmed.chars().take(63).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_names_become_valid_dns_labels() {
        assert_eq!(sanitize_instance("ismail-laptop"), "ismail-laptop");
        assert_eq!(
            sanitize_instance("Ismail's MacBook Pro"),
            "Ismail-s-MacBook-Pro"
        );
        assert_eq!(sanitize_instance("host.local"), "host-local");
        assert_eq!(sanitize_instance("---"), "splitview");
        assert_eq!(sanitize_instance(""), "splitview");
        assert!(sanitize_instance(&"x".repeat(200)).len() <= 63);
    }

    #[test]
    fn the_service_type_is_well_formed() {
        assert!(SERVICE_TYPE.starts_with('_'));
        assert!(SERVICE_TYPE.ends_with(".local."));
    }
}
