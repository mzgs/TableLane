use base64::{Engine as _, engine::general_purpose::STANDARD};
use ring::{
    aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey},
    rand::{SecureRandom, SystemRandom},
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

// shortcut: the embedded key is recoverable from the app, use an OS keychain for stronger protection.
// Keep this key stable: replacing it makes existing encrypted passwords unreadable.
const KEY: &[u8; 32] = b"\xfb\xc1\xac\x39\xf6\xf8\xd5\x99\x25\xb9\x05\xed\x1c\x56\x2b\xb0\x63\x67\xd0\x8c\xcf\x36\xe3\xdf\xe8\x30\x8f\xaf\x12\x9b\xba\x5a";
const CONTEXT: &[u8] = b"tablelane.connection.password.v1";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EncryptedPassword {
    aes256_gcm_v1: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum StoredPassword {
    Legacy(String),
    Encrypted(EncryptedPassword),
}

fn cipher() -> Result<LessSafeKey, &'static str> {
    UnboundKey::new(&AES_256_GCM, KEY)
        .map(LessSafeKey::new)
        .map_err(|_| "Couldn’t initialize password encryption")
}

pub(super) fn serialize<S: Serializer>(password: &str, serializer: S) -> Result<S::Ok, S::Error> {
    use serde::ser::Error as _;
    let mut nonce = [0; NONCE_LEN];
    SystemRandom::new()
        .fill(&mut nonce)
        .map_err(|_| S::Error::custom("Couldn’t generate a password encryption nonce"))?;
    let mut ciphertext = password.as_bytes().to_vec();
    cipher()
        .map_err(S::Error::custom)?
        .seal_in_place_append_tag(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(CONTEXT),
            &mut ciphertext,
        )
        .map_err(|_| S::Error::custom("Couldn’t encrypt password"))?;
    let mut bytes = nonce.to_vec();
    bytes.extend(ciphertext);
    EncryptedPassword {
        aes256_gcm_v1: STANDARD.encode(bytes),
    }
    .serialize(serializer)
}

pub(super) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    use serde::de::Error as _;
    let encrypted = match StoredPassword::deserialize(deserializer)? {
        StoredPassword::Legacy(password) => return Ok(password),
        StoredPassword::Encrypted(encrypted) => encrypted,
    };
    let mut bytes = STANDARD
        .decode(encrypted.aes256_gcm_v1)
        .map_err(|_| D::Error::custom("Invalid encrypted password encoding"))?;
    if bytes.len() < NONCE_LEN + AES_256_GCM.tag_len() {
        return Err(D::Error::custom("Invalid encrypted password length"));
    }
    let nonce = Nonce::try_assume_unique_for_key(&bytes[..NONCE_LEN]).map_err(D::Error::custom)?;
    let key = cipher().map_err(D::Error::custom)?;
    let plaintext = key
        .open_in_place(nonce, Aad::from(CONTEXT), &mut bytes[NONCE_LEN..])
        .map_err(|_| {
            D::Error::custom(
                "Couldn’t decrypt password: encrypted data is damaged or the key has changed",
            )
        })?;
    String::from_utf8(plaintext.to_vec())
        .map_err(|_| D::Error::custom("Decrypted password is not valid UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::{EncryptedPassword, NONCE_LEN, STANDARD};
    use base64::Engine as _;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Password {
        #[serde(with = "super")]
        password: String,
    }

    #[test]
    fn passwords_encrypt_round_trip_migrate_and_reject_damage() {
        for plaintext in ["", " secret ", "şifre🔒", "aes256_gcm_v1:legacy"] {
            let password = Password {
                password: plaintext.into(),
            };
            let first = serde_json::to_value(&password).unwrap();
            let second = serde_json::to_value(&password).unwrap();
            assert_ne!(first, second);
            assert!(first["password"].is_object());
            assert_eq!(
                serde_json::from_value::<Password>(first.clone()).unwrap(),
                password
            );
            let legacy = serde_json::json!({"password": plaintext});
            assert_eq!(
                serde_json::from_value::<Password>(legacy).unwrap(),
                password
            );
            let encrypted: EncryptedPassword =
                serde_json::from_value(first["password"].clone()).unwrap();
            let bytes = STANDARD.decode(encrypted.aes256_gcm_v1).unwrap();
            for ix in [0, NONCE_LEN, bytes.len() - 1] {
                let mut damaged = bytes.clone();
                damaged[ix] ^= 1;
                let value =
                    serde_json::json!({"password": {"aes256_gcm_v1": STANDARD.encode(damaged)}});
                assert!(serde_json::from_value::<Password>(value).is_err());
            }
        }
        for invalid in ["!", "", "AAAA"] {
            let value = serde_json::json!({"password": {"aes256_gcm_v1": invalid}});
            assert!(serde_json::from_value::<Password>(value).is_err());
        }
        assert!(
            serde_json::from_value::<Password>(
                serde_json::json!({"password": {"aes256_gcm_v2": "AAAA"}})
            )
            .is_err()
        );
    }
}
