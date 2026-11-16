use std::sync::OnceLock;

use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

const PASSWORD_ALPHABET: &[u8] = b"abcdefghijkmnpqrstuvwxyzACDEFGHJKLMNPQRTUVWXY346789";
const PASSWORD_LENGTH: usize = 24;

pub fn random_bytes<const N: usize>() -> anyhow::Result<[u8; N]> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).map_err(|error| anyhow::anyhow!("random bytes: {error}"))?;
    Ok(bytes)
}

pub fn random_token<const N: usize>() -> anyhow::Result<String> {
    Ok(URL_SAFE_NO_PAD.encode(random_bytes::<N>()?))
}

pub fn generate_password() -> anyhow::Result<String> {
    let alphabet = PASSWORD_ALPHABET.len();
    let unbiased_limit = 256 - 256 % alphabet;
    let mut password = String::with_capacity(PASSWORD_LENGTH);
    while password.len() < PASSWORD_LENGTH {
        for byte in random_bytes::<32>()? {
            let byte = usize::from(byte);
            if byte < unbiased_limit && password.len() < PASSWORD_LENGTH {
                password.push(char::from(PASSWORD_ALPHABET[byte % alphabet]));
            }
        }
    }
    Ok(password)
}

pub fn hash_password(password: &str) -> anyhow::Result<String> {
    let salt = SaltString::encode_b64(&random_bytes::<16>()?)
        .map_err(|error| anyhow::anyhow!("password salt: {error}"))?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|error| anyhow::anyhow!("hash password: {error}"))
}

pub fn verify_password(password: &str, hash: Option<&str>) -> bool {
    let known = hash.is_some();
    let hash = hash.unwrap_or_else(|| dummy_hash());
    let matches = PasswordHash::new(hash)
        .map(|parsed| {
            Argon2::default()
                .verify_password(password.as_bytes(), &parsed)
                .is_ok()
        })
        .unwrap_or(false);
    known && matches
}

fn dummy_hash() -> &'static str {
    static DUMMY: OnceLock<String> = OnceLock::new();
    DUMMY.get_or_init(|| hash_password("unknown guest").unwrap_or_default())
}

pub fn token_hash(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_long_unambiguous_passwords() {
        let password = generate_password().unwrap();
        assert_eq!(password.len(), PASSWORD_LENGTH);
        assert!(
            password
                .bytes()
                .all(|byte| PASSWORD_ALPHABET.contains(&byte))
        );
        assert_ne!(password, generate_password().unwrap());
    }

    #[test]
    fn verifies_only_the_right_password() {
        let hash = hash_password("correct horse").unwrap();
        assert!(verify_password("correct horse", Some(&hash)));
        assert!(!verify_password("wrong horse", Some(&hash)));
        assert!(!verify_password("correct horse", None));
    }
}
