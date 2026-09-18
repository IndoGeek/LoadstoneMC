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

/// Decrypt the client's session key (encrypted with our public key using
/// PKCS#1 v1.5 padding, matching Java's `Cipher.getInstance("RSA")`).
pub fn decrypt_shared_secret(private: &RsaPrivateKey, encrypted: &[u8]) -> Result<Vec<u8>, String> {
    private
        .decrypt(Pkcs1v15Encrypt, encrypted)
        .map_err(|e| e.to_string())
}

/// Compute the `serverId` hash sent to Mojang's `hasJoined` endpoint.
///
/// Matches Java's `Crypt.digestData(serverId, publicKey, sharedSecret)`:
/// `SHA-1(serverId ++ sharedSecret ++ publicKey)` hex-encoded with a `-`
/// inserted after every two characters.
pub fn compute_server_id(shared_secret: &[u8], public_key_der: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(shared_secret);
    hasher.update(public_key_der);
    let digest = hasher.finalize();

    let mut out = String::with_capacity(digest.len() * 3);
    for byte in digest {
        out.push_str(&format!("{byte:02x}-"));
    }
    out
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
