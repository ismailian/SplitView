//! Pairing-code lifecycle, the QR payload, and connection rate limiting
//! (`docs/remote-control-desktop.md` §7.2, §7.3).
//!
//! The code is the *second* factor of the pairing handshake, not the only one:
//! the client has already verified the pinned certificate before it sends the
//! code, and the desktop user still has to approve the device by name. The code
//! is what stops a device that somehow reached the socket from queueing an
//! approval prompt at all.

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

use rand::RngCore;
use subtle::ConstantTimeEq;

/// Crockford base32: no `I`, `L`, `O` or `U`, so a code read off a screen and
/// typed on a phone keyboard has no ambiguous characters.
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Code length. 8 symbols of a 32-symbol alphabet is 40 bits, which is far more
/// than a 5-attempt, 120-second window can be walked through.
const CODE_LEN: usize = 8;

/// How long a pairing code lives.
pub const CODE_TTL: Duration = Duration::from_secs(120);

/// Wrong guesses before the code is burned.
const MAX_ATTEMPTS: u32 = 5;

/// What happened when a client presented a code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeCheck {
    /// The code matched and has been consumed.
    Accepted,
    /// No code is active, or it timed out.
    Expired,
    /// Wrong code; the code is still live and the client may retry.
    Rejected,
    /// Too many wrong guesses; the code is burned and a new one is needed.
    Burned,
}

/// The single in-flight pairing code. There is at most one at a time: pairing is
/// a deliberate, user-initiated act, not a standing invitation.
#[derive(Debug, Default)]
pub struct Pairing {
    active: Option<ActiveCode>,
}

#[derive(Debug)]
struct ActiveCode {
    code: String,
    issued: Instant,
    attempts: u32,
}

impl Pairing {
    /// Issue a fresh single-use code, replacing any previous one.
    pub fn start(&mut self) -> String {
        let code = generate_code();
        self.active = Some(ActiveCode {
            code: code.clone(),
            issued: Instant::now(),
            attempts: 0,
        });
        code
    }

    /// Drop the active code (the user closed the pairing dialog).
    pub fn cancel(&mut self) {
        self.active = None;
    }

    /// The live code and how long it has left, for the Settings countdown.
    pub fn current(&self) -> Option<(String, Duration)> {
        let active = self.active.as_ref()?;
        let elapsed = active.issued.elapsed();
        if elapsed >= CODE_TTL {
            return None;
        }
        Some((active.code.clone(), CODE_TTL - elapsed))
    }

    /// Check a presented code in constant time.
    ///
    /// On success the code is consumed immediately — before the desktop user is
    /// even asked — so a second client cannot ride the same code while the
    /// approval prompt is open.
    pub fn check(&mut self, presented: &str) -> CodeCheck {
        let Some(active) = self.active.as_mut() else {
            return CodeCheck::Expired;
        };
        if active.issued.elapsed() >= CODE_TTL {
            self.active = None;
            return CodeCheck::Expired;
        }

        let normalized = normalize(presented);
        let matches: bool = normalized.as_bytes().ct_eq(active.code.as_bytes()).into();
        // A length mismatch makes `ct_eq` return false without comparing, which
        // is fine: the length of the code is public.
        if matches && normalized.len() == active.code.len() {
            self.active = None;
            return CodeCheck::Accepted;
        }

        active.attempts += 1;
        if active.attempts >= MAX_ATTEMPTS {
            self.active = None;
            CodeCheck::Burned
        } else {
            CodeCheck::Rejected
        }
    }
}

/// Accept a code typed with lowercase letters or grouping spaces/dashes, and
/// fold Crockford's ambiguous characters onto their canonical ones.
fn normalize(input: &str) -> String {
    input
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| match c.to_ascii_uppercase() {
            'I' | 'L' => '1',
            'O' => '0',
            'U' => 'V',
            other => other,
        })
        .collect()
}

fn generate_code() -> String {
    let mut bytes = [0u8; CODE_LEN];
    rand::thread_rng().fill_bytes(&mut bytes);
    // Rejection-free mapping: the alphabet is exactly 32 symbols, so masking the
    // low 5 bits of a uniform byte is itself uniform.
    bytes
        .iter()
        .map(|b| ALPHABET[(b & 0x1f) as usize] as char)
        .collect()
}

/// Build the QR payload the phone scans (`docs/remote-control-desktop.md` §7.2).
///
/// Everything in it is public except the code, which is short-lived and
/// single-use: the QR is on the screen of the laptop the user is sitting at.
pub fn qr_payload(host: &str, port: u16, fingerprint_b64: &str, code: &str) -> String {
    format!("splitview://pair?h={host}&p={port}&f={fingerprint_b64}&c={code}&v=1")
}

/// Per-IP connection throttle (§7.3: 10 attempts per minute).
///
/// Deliberately crude: a fixed window over a small map, cleaned as it is used.
/// This exists to blunt a script, not to survive a distributed attacker — the
/// pairing code and the approval prompt are what actually stand in the way.
#[derive(Debug)]
pub struct RateLimiter {
    window: Duration,
    limit: u32,
    seen: HashMap<IpAddr, (Instant, u32)>,
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new(Duration::from_secs(60), 10)
    }
}

impl RateLimiter {
    pub fn new(window: Duration, limit: u32) -> Self {
        Self {
            window,
            limit,
            seen: HashMap::new(),
        }
    }

    /// Record a connection attempt. `false` means "over the limit, drop it".
    pub fn allow(&mut self, ip: IpAddr) -> bool {
        let now = Instant::now();
        self.seen
            .retain(|_, (started, _)| now.duration_since(*started) < self.window);

        let entry = self.seen.entry(ip).or_insert((now, 0));
        if now.duration_since(entry.0) >= self.window {
            *entry = (now, 0);
        }
        entry.1 += 1;
        entry.1 <= self.limit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issues_an_unambiguous_code_of_the_right_shape() {
        let mut pairing = Pairing::default();
        let code = pairing.start();
        assert_eq!(code.len(), CODE_LEN);
        assert!(code.chars().all(|c| ALPHABET.contains(&(c as u8))));
        // Crockford's excluded characters must never appear.
        assert!(!code.contains(['I', 'L', 'O', 'U']));
    }

    #[test]
    fn accepts_the_right_code_exactly_once() {
        let mut pairing = Pairing::default();
        let code = pairing.start();
        assert_eq!(pairing.check(&code), CodeCheck::Accepted);
        // Single-use: a replay finds nothing to match against.
        assert_eq!(pairing.check(&code), CodeCheck::Expired);
    }

    #[test]
    fn accepts_a_code_typed_loosely() {
        let mut pairing = Pairing::default();
        let code = pairing.start();
        let typed = format!("{}-{}", &code[..4], &code[4..]).to_lowercase();
        assert_eq!(pairing.check(&typed), CodeCheck::Accepted);
    }

    #[test]
    fn folds_crockford_lookalikes_onto_their_canonical_characters() {
        assert_eq!(normalize("i l o u"), "110V");
        assert_eq!(normalize("K7m2-qx4z"), "K7M2QX4Z");
    }

    #[test]
    fn burns_the_code_after_five_wrong_guesses() {
        let mut pairing = Pairing::default();
        let code = pairing.start();
        for _ in 0..(MAX_ATTEMPTS - 1) {
            assert_eq!(pairing.check("00000000"), CodeCheck::Rejected);
        }
        assert_eq!(pairing.check("00000000"), CodeCheck::Burned);
        // Even the right code is worthless now.
        assert_eq!(pairing.check(&code), CodeCheck::Expired);
    }

    #[test]
    fn reports_expired_when_no_code_is_active() {
        let mut pairing = Pairing::default();
        assert_eq!(pairing.check("K7M2QX4Z"), CodeCheck::Expired);
        assert!(pairing.current().is_none());
    }

    #[test]
    fn cancelling_drops_the_code() {
        let mut pairing = Pairing::default();
        let code = pairing.start();
        assert!(pairing.current().is_some());
        pairing.cancel();
        assert!(pairing.current().is_none());
        assert_eq!(pairing.check(&code), CodeCheck::Expired);
    }

    #[test]
    fn starting_again_replaces_the_previous_code() {
        let mut pairing = Pairing::default();
        let first = pairing.start();
        let second = pairing.start();
        assert_ne!(first, second);
        assert_eq!(pairing.check(&first), CodeCheck::Rejected);
        assert_eq!(pairing.check(&second), CodeCheck::Accepted);
    }

    #[test]
    fn qr_payload_carries_everything_the_client_needs() {
        let payload = qr_payload("192.168.1.20", 8765, "AbC-_123", "K7M2QX4Z");
        assert_eq!(
            payload,
            "splitview://pair?h=192.168.1.20&p=8765&f=AbC-_123&c=K7M2QX4Z&v=1"
        );
    }

    #[test]
    fn rate_limiter_allows_a_burst_then_refuses() {
        let mut limiter = RateLimiter::new(Duration::from_secs(60), 3);
        let ip: IpAddr = "192.168.1.44".parse().expect("ip");
        assert!(limiter.allow(ip));
        assert!(limiter.allow(ip));
        assert!(limiter.allow(ip));
        assert!(!limiter.allow(ip));
        // A different device is unaffected by the first one's noise.
        assert!(limiter.allow("192.168.1.45".parse().expect("ip")));
    }

    #[test]
    fn rate_limiter_forgets_an_elapsed_window() {
        let mut limiter = RateLimiter::new(Duration::from_millis(0), 1);
        let ip: IpAddr = "10.0.0.1".parse().expect("ip");
        assert!(limiter.allow(ip));
        // A zero-length window has always already elapsed, so the count resets.
        assert!(limiter.allow(ip));
    }
}
