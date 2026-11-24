//! SCRAM handshake helpers and per-IP attempt gating.

use rust_chat_crypto::scram::{auth_message, server_signature, verify_proof};
use rust_chat_crypto::{
    PasswordVerifier, base64_decode, base64_encode, random_bytes, sha256, to_hex,
};
use rust_chat_protocol::KdfParams;
use rust_chat_protocol::payloads::{Auth, Challenge};
use rust_chat_protocol::{ErrorCode, MAX_NAME_CHARS};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthFailure {
    pub code: ErrorCode,
    pub message: String,
}

impl AuthFailure {
    fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// 1–32 characters, no control characters, no leading/trailing whitespace.
pub fn validate_name(raw: &str) -> Result<String, AuthFailure> {
    let count = raw.chars().count();
    if count == 0 || count > MAX_NAME_CHARS {
        return Err(AuthFailure::new(
            ErrorCode::NameInvalid,
            format!("display name must be 1-{MAX_NAME_CHARS} characters"),
        ));
    }
    if raw != raw.trim() {
        return Err(AuthFailure::new(
            ErrorCode::NameInvalid,
            "display name must not have leading or trailing whitespace",
        ));
    }
    if raw.chars().any(char::is_control) {
        return Err(AuthFailure::new(
            ErrorCode::NameInvalid,
            "display name must not contain control characters",
        ));
    }
    Ok(raw.to_owned())
}

/// `SHA256:` plus the lowercase hex of the Ed25519 public key.
pub fn pubkey_fingerprint(pubkey: &str) -> Result<String, AuthFailure> {
    let bytes = base64_decode(pubkey)
        .map_err(|_| AuthFailure::new(ErrorCode::BadPayload, "pubkey must be base64"))?;
    if bytes.len() != 32 {
        return Err(AuthFailure::new(
            ErrorCode::BadPayload,
            "pubkey must be a 32-byte Ed25519 key",
        ));
    }
    Ok(format!("SHA256:{}", to_hex(&sha256(&bytes))))
}

pub fn challenge(kdf: &KdfParams) -> (Challenge, Vec<u8>) {
    let nonce = random_bytes(16);
    (
        Challenge {
            nonce: base64_encode(&nonce),
            kdf: kdf.clone(),
        },
        nonce,
    )
}

/// Verifies the client's proof and returns the server signature to put in `welcome`.
pub fn verify(
    verifier: &PasswordVerifier,
    name: &str,
    server_nonce: &[u8],
    auth: &Auth,
) -> Result<[u8; 32], AuthFailure> {
    let client_nonce = base64_decode(&auth.client_nonce)
        .map_err(|_| AuthFailure::new(ErrorCode::BadPayload, "clientNonce must be base64"))?;
    let proof = base64_decode(&auth.proof)
        .map_err(|_| AuthFailure::new(ErrorCode::BadPayload, "proof must be base64"))?;
    if client_nonce.len() != 16 {
        return Err(AuthFailure::new(
            ErrorCode::BadPayload,
            "clientNonce must be 16 bytes",
        ));
    }
    let message = auth_message(name, server_nonce, &client_nonce);
    if !verify_proof(&proof, &verifier.stored_key, &message) {
        return Err(AuthFailure::new(ErrorCode::AuthFailed, "invalid password"));
    }
    Ok(server_signature(&verifier.server_key, &message))
}

/// Three failed attempts per minute, then a 30 s lockout. Successful connections do not
/// consume attempts, so a reconnect loop is never locked out by its own logins.
pub struct AuthGate {
    failures: Vec<Instant>,
    locked_until: Option<Instant>,
}

impl Default for AuthGate {
    fn default() -> Self {
        Self::new()
    }
}

impl AuthGate {
    pub fn new() -> Self {
        Self {
            failures: Vec::new(),
            locked_until: None,
        }
    }

    pub fn allow(&mut self, now: Instant) -> bool {
        if let Some(until) = self.locked_until {
            if now < until {
                return false;
            }
            self.locked_until = None;
        }
        true
    }

    pub fn fail(&mut self, now: Instant) {
        self.failures
            .retain(|failure| now.saturating_duration_since(*failure) < Duration::from_secs(60));
        self.failures.push(now);
        if self.failures.len() >= 3 {
            self.failures.clear();
            self.locked_until = Some(now + Duration::from_secs(30));
        }
    }

    pub fn success(&mut self) {
        self.failures.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_validated() {
        assert!(validate_name("alice").is_ok());
        assert!(validate_name("").is_err());
        assert!(validate_name(" alice").is_err());
        assert!(validate_name("alice\n").is_err());
        assert!(validate_name(&"a".repeat(MAX_NAME_CHARS + 1)).is_err());
        assert!(validate_name(&"a".repeat(MAX_NAME_CHARS)).is_ok());
    }

    #[test]
    fn fingerprints_need_a_real_ed25519_key() {
        let key = base64_encode(&[7u8; 32]);
        let fp = pubkey_fingerprint(&key).unwrap();
        assert!(fp.starts_with("SHA256:"));
        assert_eq!(fp.len(), 7 + 64);
        assert!(pubkey_fingerprint(&base64_encode(&[0u8; 16])).is_err());
        assert!(pubkey_fingerprint("not base64").is_err());
    }

    #[test]
    fn auth_gate_locks_out_after_three_failures() {
        let now = Instant::now();
        let mut gate = AuthGate::new();
        assert!(gate.allow(now));
        gate.fail(now);
        assert!(gate.allow(now));
        gate.fail(now);
        assert!(gate.allow(now));
        gate.fail(now);
        assert!(!gate.allow(now + Duration::from_secs(10)));
        assert!(gate.allow(now + Duration::from_secs(31)));
    }

    #[test]
    fn successful_logins_do_not_consume_attempts() {
        let now = Instant::now();
        let mut gate = AuthGate::new();
        for _ in 0..10 {
            assert!(gate.allow(now));
            gate.success();
        }
        gate.fail(now);
        gate.fail(now);
        gate.success();
        gate.fail(now);
        assert!(gate.allow(now));
    }
}
