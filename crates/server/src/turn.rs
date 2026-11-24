//! TURN REST credentials (the coturn `static-auth-secret` scheme).

use crate::config::TurnConfig;
use hmac::digest::KeyInit;
use hmac::{Hmac, Mac};
use rust_chat_crypto::base64_encode;
use rust_chat_protocol::payloads::TurnCredentials;
use sha1::Sha1;

pub fn credentials(config: &TurnConfig, session_id: &str) -> TurnCredentials {
    let ttl = config.ttl_seconds.max(60);
    let expiry = now_secs().saturating_add(ttl as i64);
    let username = format!("{expiry}:{session_id}");
    let credential = base64_encode(&hmac_sha1(config.secret.as_bytes(), username.as_bytes()));
    TurnCredentials {
        urls: config.urls.clone(),
        username,
        credential,
        ttl_seconds: ttl,
    }
}

pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

fn hmac_sha1(key: &[u8], message: &[u8]) -> [u8; 20] {
    let mut mac = Hmac::<Sha1>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(message);
    mac.finalize().into_bytes().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_have_the_coturn_shape() {
        let config = TurnConfig {
            urls: vec!["turn:example.org:3478?transport=udp".into()],
            secret: "shared secret".into(),
            ttl_seconds: 3600,
        };
        let credentials = credentials(&config, "s_abc");
        assert_eq!(credentials.urls.len(), 1);
        assert_eq!(credentials.ttl_seconds, 3600);
        let (expiry, session) = credentials.username.split_once(':').unwrap();
        assert_eq!(session, "s_abc");
        assert!(expiry.parse::<i64>().unwrap() > now_secs());
        assert!(!credentials.credential.is_empty());
    }

    #[test]
    fn credential_is_stable_for_the_same_username() {
        let config = TurnConfig {
            urls: Vec::new(),
            secret: "secret".into(),
            ttl_seconds: 60,
        };
        let username = "123:s_x";
        let a = base64_encode(&hmac_sha1(config.secret.as_bytes(), username.as_bytes()));
        let b = base64_encode(&hmac_sha1(config.secret.as_bytes(), username.as_bytes()));
        assert_eq!(a, b);
    }
}
