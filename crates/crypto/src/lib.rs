//! Password verifier, SCRAM-SHA-256 and certificate fingerprints.
//!
//! The verifier file format is byte-compatible with the Kotlin server, so an existing
//! `config/verifier` can be reused when migrating a deployment.

pub mod scram;
pub mod verifier;

pub use scram::{KdfParams, ScramKeys};
pub use verifier::{PasswordVerifier, load_verifier, save_verifier};

use base64::Engine as _;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CryptoError {
    #[error("Argon2id failed: {0}")]
    Argon2(#[from] argon2::Error),
    #[error("invalid base64: {0}")]
    Base64(#[from] base64::DecodeError),
    #[error("password must not be empty")]
    EmptyPassword,
    #[error("verifier file is malformed: {0}")]
    MalformedVerifier(String),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, CryptoError>;

/// 16 random bytes, base64 on the wire.
pub fn random_bytes(count: usize) -> Vec<u8> {
    use rand::RngExt;
    let mut bytes = vec![0u8; count];
    rand::rng().fill(&mut bytes[..]);
    bytes
}

pub fn base64_encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

pub fn base64_decode(text: &str) -> Result<Vec<u8>> {
    Ok(base64::engine::general_purpose::STANDARD.decode(text)?)
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

/// HMAC-SHA-256; any key length is valid for HMAC.
pub fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    use hmac::digest::KeyInit;
    use hmac::{Hmac, Mac};
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(message);
    mac.finalize().into_bytes().into()
}

pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.ct_eq(b).into()
}

pub fn to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// `AA:BB:CC:...`, uppercase, the shape clients show and pin.
pub fn to_fingerprint(bytes: &[u8]) -> String {
    to_hex(bytes)
        .as_bytes()
        .chunks(2)
        .map(|pair| {
            std::str::from_utf8(pair)
                .expect("hex is ASCII")
                .to_uppercase()
        })
        .collect::<Vec<_>>()
        .join(":")
}

/// SHA-256 of a DER certificate, formatted for comparison with what the server prints.
pub fn certificate_fingerprint(der: &[u8]) -> String {
    to_fingerprint(&sha256(der))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_grouped_uppercase_hex() {
        let value = to_fingerprint(&[0x00, 0xab, 0xff]);
        assert_eq!(value, "00:AB:FF");
        assert_eq!(value.len(), 8);
    }

    #[test]
    fn hmac_and_sha_are_stable() {
        // SHA-256("abc")
        let digest = to_hex(&sha256(b"abc"));
        assert_eq!(
            digest,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // HMAC-SHA-256 with key "key" over "The quick brown fox jumps over the lazy dog"
        let mac = to_hex(&hmac_sha256(
            b"key",
            b"The quick brown fox jumps over the lazy dog",
        ));
        assert_eq!(
            mac,
            "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
        );
    }

    #[test]
    fn constant_time_compare_matches_equality() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }
}
