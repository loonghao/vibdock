//! Non-secret discovery challenges. A status code alone cannot prove that a
//! loopback listener owns the instance token, and discovery must not disclose
//! that token to a process which has inherited the recorded port.

use hmac::{Hmac, Mac};
use sha2::Sha256;

const DOMAIN: &[u8] = b"VibDock instance proof v1\0";

fn mac(token: &str, nonce: &str) -> Option<Hmac<Sha256>> {
    if nonce.len() != 64 || !nonce.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(token.as_bytes()).ok()?;
    mac.update(DOMAIN);
    mac.update(nonce.as_bytes());
    Some(mac)
}

pub(crate) fn respond(token: &str, nonce: &str) -> Option<String> {
    let digest = mac(token, nonce)?.finalize().into_bytes();
    Some(hex(&digest))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut output, byte| {
        use std::fmt::Write;
        let _ = write!(output, "{byte:02x}");
        output
    })
}

/// Public presentation identity only. This separate HMAC domain is never an
/// instance proof, selection capability, or permission to act on an application.
pub(crate) fn observation_id(salt: Option<&str>, kind: &str, identity: &str) -> Option<String> {
    let salt =
        salt.filter(|salt| salt.len() == 64 && salt.bytes().all(|byte| byte.is_ascii_hexdigit()))?;
    if !matches!(
        kind,
        "windows.catalog" | "windows.playback" | "macos.application"
    ) || identity.is_empty()
    {
        return None;
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(salt.as_bytes()).ok()?;
    mac.update(b"VibDock observation v1\0");
    mac.update(kind.as_bytes());
    mac.update(&[0]);
    mac.update(identity.as_bytes());
    Some(hex(&mac.finalize().into_bytes()))
}

pub(crate) fn verify(token: &str, nonce: &str, proof: &str) -> bool {
    let Some(mac) = mac(token, nonce) else {
        return false;
    };
    if proof.len() != 64 || !proof.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return false;
    }
    let bytes: Option<Vec<u8>> = (0..64)
        .step_by(2)
        .map(|offset| u8::from_str_radix(&proof[offset..offset + 2], 16).ok())
        .collect();
    bytes.is_some_and(|bytes| mac.verify_slice(&bytes).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observation_ids_are_instance_and_kind_scoped_and_never_proofs() {
        let salt = "a".repeat(64);
        let identity = "private process identity";
        let id = observation_id(Some(&salt), "windows.catalog", identity).unwrap();
        assert_eq!(
            id,
            observation_id(Some(&salt), "windows.catalog", identity).unwrap()
        );
        assert_ne!(
            id,
            observation_id(Some(&"b".repeat(64)), "windows.catalog", identity).unwrap()
        );
        assert_ne!(
            id,
            observation_id(Some(&salt), "windows.playback", identity).unwrap()
        );
        assert_ne!(
            id,
            observation_id(Some(&salt), "macos.application", identity).unwrap()
        );
        assert_eq!(id.len(), 64);
        assert!(id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)));
        assert!(!verify(&salt, &id, &id));
        assert!(!id.contains(identity));
        assert!(observation_id(None, "windows.catalog", identity).is_none());
        assert!(observation_id(Some("weak"), "windows.catalog", identity).is_none());
        assert!(observation_id(Some(&salt), "instance proof", identity).is_none());
    }

    #[test]
    fn proof_matches_an_independent_hmac_sha256_vector() {
        assert_eq!(
            respond("synthetic-secret", &"a".repeat(64)).unwrap(),
            "61610dde960cebe3b5abbb589dc9f9538d7d7d086ef476dbfa9c904bd3481959"
        );
    }

    #[test]
    fn proofs_require_the_same_secret_and_fresh_challenge() {
        let nonce = "a".repeat(64);
        let proof = respond("synthetic-secret", &nonce).unwrap();
        assert!(verify("synthetic-secret", &nonce, &proof));
        assert!(!verify("another-secret", &nonce, &proof));
        assert!(!verify("synthetic-secret", &"b".repeat(64), &proof));
        assert!(!verify("synthetic-secret", &nonce, &nonce));
        assert!(!proof.contains("synthetic-secret"));
    }

    #[test]
    fn malformed_challenges_and_proofs_fail_closed() {
        for nonce in [
            "".to_owned(),
            "a".repeat(63),
            "x".repeat(64),
            "é".repeat(32),
        ] {
            assert!(respond("test-secret", &nonce).is_none());
            assert!(!verify("test-secret", &nonce, &"a".repeat(64)));
        }
        for proof in [
            "".to_owned(),
            "a".repeat(63),
            "x".repeat(64),
            "é".repeat(32),
        ] {
            assert!(!verify("test-secret", &"a".repeat(64), &proof));
        }
    }
}
