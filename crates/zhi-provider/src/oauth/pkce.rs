//! Pares PKCE (RFC 7636) usados por el flujo OAuth: `verifier` aleatorio +
//! `challenge = base64url(SHA-256(verifier))`.

use base64::Engine;
use rand::RngCore;
use sha2::{Digest, Sha256};

/// Caracteres permitidos para `verifier` (`unreserved` de RFC 3986 sin `.~`).
const VERIFIER_ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ\
abcdefghijklmnopqrstuvwxyz\
0123456789\
-._~";

/// Par PKCE: `verifier` (alto-entropy) y `challenge` derivado con S256.
#[derive(Debug, Clone)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    /// Genera un par S256 con un verifier de `len` caracteres (43–128).
    pub fn generate() -> Self {
        Self::with_len(64)
    }

    pub fn with_len(len: usize) -> Self {
        let len = len.clamp(43, 128);
        let mut rng = rand::thread_rng();
        let mut bytes = vec![0u8; len];
        rng.fill_bytes(&mut bytes);
        let verifier: String = bytes
            .iter()
            .map(|b| VERIFIER_ALPHABET[(*b as usize) % VERIFIER_ALPHABET.len()] as char)
            .collect();
        let challenge = base64_url_encode(&Sha256::digest(verifier.as_bytes()));
        Self {
            verifier,
            challenge,
        }
    }
}

/// Cadena aleatoria base64url para parámetros como `state` o `nonce`.
pub fn random_state(len_bytes: usize) -> String {
    let mut bytes = vec![0u8; len_bytes];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64_url_encode(&bytes)
}

/// `base64url` sin padding (RFC 4648 §5).
pub fn base64_url_encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Decodifica `base64url` con o sin padding.
pub fn base64_url_decode(s: &str) -> Result<Vec<u8>, base64::DecodeError> {
    // Algunos tokens (JWT) traen padding implícito o explícito; se prueba con
    // padding flexible primero, luego sin.
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s.trim_end_matches('='))
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_is_s256_of_verifier() {
        let p = Pkce::generate();
        assert!(p.verifier.len() >= 43 && p.verifier.len() <= 128);
        let expected = base64_url_encode(&Sha256::digest(p.verifier.as_bytes()));
        assert_eq!(p.challenge, expected);
    }

    #[test]
    fn random_state_is_unique() {
        let a = random_state(32);
        let b = random_state(32);
        assert_ne!(a, b);
    }

    #[test]
    fn base64_url_roundtrip() {
        let raw = [0x00, 0x01, 0xff, 0xfe, 0xab];
        let encoded = base64_url_encode(&raw);
        let decoded = base64_url_decode(&encoded).unwrap();
        assert_eq!(decoded, raw);
    }
}
