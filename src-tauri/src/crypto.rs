use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use hmac::{Hmac, Mac};
use rand::RngCore;
use scrypt::{scrypt, Params};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

#[derive(Serialize, Deserialize)]
pub struct Envelope {
    nonce: String,
    data: String,
}

pub fn pairing_key(code: &str, salt: &str) -> Result<[u8; 32], String> {
    let salt = URL_SAFE_NO_PAD.decode(salt).map_err(|e| e.to_string())?;
    let mut key = [0_u8; 32];
    scrypt(
        code.as_bytes(),
        &salt,
        &Params::new(14, 8, 1, 32).map_err(|e| e.to_string())?,
        &mut key,
    )
    .map_err(|e| e.to_string())?;
    Ok(key)
}

pub fn proof(key: &[u8], requester: &str, target: &str) -> String {
    let mut mac = <HmacSha256 as Mac>::new_from_slice(key).expect("HMAC key");
    mac.update(format!("crosscopy-pair:{requester}:{target}").as_bytes());
    URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
}

pub fn encrypt<T: Serialize>(key: &[u8], value: &T) -> Result<Envelope, String> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|e| e.to_string())?;
    let mut nonce = [0_u8; 12];
    rand::rng().fill_bytes(&mut nonce);
    let plain = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    let data = cipher
        .encrypt(Nonce::from_slice(&nonce), plain.as_ref())
        .map_err(|e| e.to_string())?;
    Ok(Envelope {
        nonce: URL_SAFE_NO_PAD.encode(nonce),
        data: URL_SAFE_NO_PAD.encode(data),
    })
}

pub fn decrypt<T: DeserializeOwned>(key: &[u8], envelope: &Envelope) -> Result<T, String> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|e| e.to_string())?;
    let nonce = URL_SAFE_NO_PAD
        .decode(&envelope.nonce)
        .map_err(|e| e.to_string())?;
    let data = URL_SAFE_NO_PAD
        .decode(&envelope.data)
        .map_err(|e| e.to_string())?;
    let plain = cipher
        .decrypt(Nonce::from_slice(&nonce), data.as_ref())
        .map_err(|e| e.to_string())?;
    serde_json::from_slice(&plain).map_err(|e| e.to_string())
}

pub fn random_secret() -> String {
    let mut bytes = [0_u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn decode_secret(value: &str) -> Result<Vec<u8>, String> {
    URL_SAFE_NO_PAD.decode(value).map_err(|e| e.to_string())
}

pub fn fingerprint(data: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(data.as_ref()))
}
