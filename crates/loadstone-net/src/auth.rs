//! Authentication helpers: RSA key generation, shared-secret decryption,
//! the `serverId` hash for Mojang session verification, and the `hasJoined`
//! API lookup.

use std::time::Duration;

use rsa::pkcs8::EncodePublicKey;
use rsa::{Pkcs1v15Encrypt, RsaPrivateKey, RsaPublicKey};
use sha1::{Digest, Sha1};
use uuid::Uuid;

/// A freshly generated RSA keypair plus its SPKI DER public key encoding,
/// which is what goes on the wire in the Encryption Request packet.
pub struct ServerKeys {
    pub private: RsaPrivateKey,
    pub public_der: Vec<u8>,
}

pub fn generate_keys() -> Result<ServerKeys, String> {
    use rand::rngs::OsRng;
    let mut rng = OsRng;
    // Vanilla uses 1024-bit RSA.
    let private = RsaPrivateKey::new(&mut rng, 1024).map_err(|e| e.to_string())?;
    let public: RsaPublicKey = private.to_public_key();
    let public_der = public
        .to_public_key_der()
        .map_err(|e| e.to_string())?
        .as_bytes()
        .to_vec();
    Ok(ServerKeys {
        private,
        public_der,
    })
}

/// Decrypt a value the client encrypted with our public key, using PKCS#1 v1.5
/// padding to match Java's `Cipher.getInstance("RSA")`.
///
/// Used for both halves of the Encryption Response: the 16-byte session key and
/// the echoed verify token.
pub fn decrypt_with_private_key(
    private: &RsaPrivateKey,
    encrypted: &[u8],
) -> Result<Vec<u8>, String> {
    private
        .decrypt(Pkcs1v15Encrypt, encrypted)
        .map_err(|e| e.to_string())
}

/// Compute the `serverId` hash sent to Mojang's `hasJoined` endpoint.
///
/// Matches Java's `new BigInteger(Crypt.digestData(serverId, publicKey, secret)).toString(16)`
/// with an empty `serverId`, which vanilla sends: `SHA-1(sharedSecret ++ publicKey)`
/// read as a **signed** big-endian integer, so the hex is Java's `toString(16)`
/// of that integer rather than a hex dump of the bytes.
///
/// The distinction is not cosmetic. A digest whose first bit is set is negative
/// to Java, which prints a two's-complement value with a leading `-` and drops
/// leading zero bytes; hex-encoding byte by byte (and joining with `-`, the
/// pre-1.7 "session id" look) produces a string no modern client ever sent, and
/// `hasJoined` answers that with 204.
pub fn compute_server_id(shared_secret: &[u8], public_key_der: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(shared_secret);
    hasher.update(public_key_der);
    let digest = hasher.finalize();
    java_hex(&digest)
}

/// Java's `new BigInteger(bytes).toString(16)`: signed, two's-complement, lower
/// case, leading zeros dropped, and `0` (not an empty string) for zero.
fn java_hex(bytes: &[u8]) -> String {
    let negative = bytes.first().is_some_and(|byte| byte & 0x80 != 0);
    let mut magnitude = bytes.to_vec();
    if negative {
        // Two's complement: invert every byte, then add one.
        let mut carry = true;
        for byte in magnitude.iter_mut().rev() {
            let inverted = !*byte;
            if carry {
                carry = inverted == 0xff;
                *byte = if carry { 0 } else { inverted + 1 };
            } else {
                *byte = inverted;
            }
        }
    }

    let mut hex = magnitude
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let trimmed = hex.trim_start_matches('0');
    hex = if trimmed.is_empty() {
        "0".to_string()
    } else {
        trimmed.to_string()
    };

    if negative {
        hex.insert(0, '-');
    }
    hex
}

#[derive(Debug, Clone)]
pub struct SessionProfile {
    pub uuid: Uuid,
    pub name: String,
}

#[derive(serde::Deserialize)]
struct SessionResponse {
    id: String,
    name: String,
}

/// Query Mojang's `hasJoined` endpoint (or a mock) to prove the client owns
/// its username. Returns `None` if auth failed (bad key exchange, offline
/// attacker, or a cracked client).
pub async fn check_session(
    sessionserver_url: &str,
    username: &str,
    server_id: &str,
) -> Option<SessionProfile> {
    let url = format!("{sessionserver_url}?username={username}&serverId={server_id}");
    tokio::task::spawn_blocking(move || {
        let body = ureq::get(&url)
            .timeout(Duration::from_secs(8))
            .call()
            .ok()?
            .into_string()
            .ok()?;
        let response: SessionResponse = serde_json::from_str(&body).ok()?;
        let uuid = Uuid::parse_str(&response.id).ok()?;
        Some(SessionProfile {
            uuid,
            name: response.name,
        })
    })
    .await
    .ok()
    .flatten()
}

pub fn random_bytes<const N: usize>() -> [u8; N] {
    use rand::RngCore;
    let mut bytes = [0u8; N];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes
}

/// `compute_server_id` values verified against `new BigInteger(sha1).toString(16)`
/// in Java itself, plus the edge cases of that conversion. The two full vectors
/// use a real two-part SHA-1 input, so they also pin the byte order of the hash.
#[cfg(test)]
mod tests {
    use super::*;

    fn secret() -> Vec<u8> {
        (0u8..16).collect()
    }

    fn key() -> Vec<u8> {
        let mut key = vec![0x30, 0x82, 0x01];
        key.extend([b'k'; 40]);
        key
    }

    #[test]
    fn server_id_hash_matches_javas_signed_big_integer_form() {
        // sha1(secret ++ key) starts with 0xeb, so Java reads it as negative.
        assert_eq!(
            compute_server_id(&secret(), &key()),
            "-141e02509cfd051627c9982364064e0b919730fa"
        );
    }

    #[test]
    fn a_positive_digest_is_hex_without_a_sign() {
        let secret: Vec<u8> = (0x10u8..0x20).collect();
        let key: Vec<u8> = (0u8..64).map(|i| 14 + i).collect();
        assert_eq!(
            compute_server_id(&secret, &key),
            "2453489d9395cdc8f03700840a4bf0f3161171e5"
        );
    }

    #[test]
    fn leading_zero_bytes_are_dropped_like_javas_to_string() {
        assert_eq!(java_hex(&[0, 0, 0x7f, 0x01]), "7f01");
        assert_eq!(java_hex(&[0; 20]), "0");
    }

    #[test]
    fn a_negative_digest_is_two_s_complement() {
        assert_eq!(java_hex(&[0x80, 0x00]), "-8000");
        assert_eq!(java_hex(&[0xff, 0xff]), "-1");
    }
}
