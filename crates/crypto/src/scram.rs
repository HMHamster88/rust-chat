//! SCRAM-SHA-256 over an Argon2id-derived key. Byte layout mirrors `docs/PROTOCOL.md` §2.

use crate::{CryptoError, Result, base64_decode, constant_time_eq, hmac_sha256, sha256};
use argon2::{Algorithm, Argon2, Params, Version};

pub use rust_chat_protocol::KdfParams;

pub const CLIENT_KEY_LABEL: &[u8] = b"client key";
pub const SERVER_KEY_LABEL: &[u8] = b"server key";

#[derive(Debug, Clone)]
pub struct ScramKeys {
    pub client_key: [u8; 32],
    pub stored_key: [u8; 32],
    pub server_key: [u8; 32],
}

/// Argon2id(password, salt, m, t, p) -> 32 bytes.
pub fn derive_auth_key(password: &str, kdf: &KdfParams) -> Result<[u8; 32]> {
    if password.is_empty() {
        return Err(CryptoError::EmptyPassword);
    }
    let salt = base64_decode(&kdf.salt)?;
    let params = Params::new(kdf.m, kdf.t, kdf.p, Some(32))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut out = [0u8; 32];
    argon.hash_password_into(password.as_bytes(), &salt, &mut out)?;
    Ok(out)
}

pub fn keys(auth_key: &[u8; 32]) -> ScramKeys {
    let client_key = hmac_sha256(auth_key, CLIENT_KEY_LABEL);
    ScramKeys {
        client_key,
        stored_key: sha256(&client_key),
        server_key: hmac_sha256(auth_key, SERVER_KEY_LABEL),
    }
}

/// `UTF8(name) 0x00 serverNonce 0x00 clientNonce`.
pub fn auth_message(name: &str, server_nonce: &[u8], client_nonce: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(name.len() + server_nonce.len() + client_nonce.len() + 2);
    message.extend_from_slice(name.as_bytes());
    message.push(0);
    message.extend_from_slice(server_nonce);
    message.push(0);
    message.extend_from_slice(client_nonce);
    message
}

pub fn client_proof(keys: &ScramKeys, auth_message: &[u8]) -> [u8; 32] {
    let signature = hmac_sha256(&keys.stored_key, auth_message);
    let mut proof = [0u8; 32];
    for index in 0..32 {
        proof[index] = keys.client_key[index] ^ signature[index];
    }
    proof
}

pub fn verify_proof(proof: &[u8], stored_key: &[u8; 32], auth_message: &[u8]) -> bool {
    if proof.len() != 32 {
        return false;
    }
    let signature = hmac_sha256(stored_key, auth_message);
    let mut recovered = [0u8; 32];
    for index in 0..32 {
        recovered[index] = proof[index] ^ signature[index];
    }
    constant_time_eq(&sha256(&recovered), stored_key)
}

pub fn server_signature(server_key: &[u8; 32], auth_message: &[u8]) -> [u8; 32] {
    hmac_sha256(server_key, auth_message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fast_kdf() -> KdfParams {
        KdfParams {
            alg: "argon2id".into(),
            salt: crate::base64_encode(&[7u8; 16]),
            m: 8,
            t: 1,
            p: 1,
        }
    }

    #[test]
    fn correct_password_proves_and_verifies() {
        let kdf = fast_kdf();
        let auth_key = derive_auth_key("correct horse battery staple", &kdf).unwrap();
        let keys = keys(&auth_key);
        let name = "alice";
        let server_nonce = [1u8; 16];
        let client_nonce = [2u8; 16];
        let message = auth_message(name, &server_nonce, &client_nonce);
        let proof = client_proof(&keys, &message);

        assert!(verify_proof(&proof, &keys.stored_key, &message));
        assert_ne!(keys.server_key, keys.stored_key);
        assert_ne!(keys.client_key, keys.stored_key);
    }

    #[test]
    fn wrong_password_and_tampering_are_rejected() {
        let kdf = fast_kdf();
        let good = keys(&derive_auth_key("good", &kdf).unwrap());
        let bad = keys(&derive_auth_key("bad", &kdf).unwrap());
        let message = auth_message("alice", &[1u8; 16], &[2u8; 16]);
        let proof = client_proof(&bad, &message);

        assert!(!verify_proof(&proof, &good.stored_key, &message));

        let proof = client_proof(&good, &message);
        let mut tampered = message.clone();
        tampered.push(b'x');
        assert!(!verify_proof(&proof, &good.stored_key, &tampered));
    }

    #[test]
    fn auth_message_layout_is_exact() {
        let message = auth_message("ab", &[0xaa], &[0xbb]);
        assert_eq!(message, vec![b'a', b'b', 0, 0xaa, 0, 0xbb]);
    }

    #[test]
    fn empty_password_is_refused() {
        assert!(derive_auth_key("", &fast_kdf()).is_err());
    }
}
