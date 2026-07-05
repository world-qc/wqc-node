use ed25519_dalek::{SigningKey, VerifyingKey};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

const HKDF_INFO: &[u8] = b"wqc-operator-v1";

type HmacSha256 = Hmac<Sha256>;

/// Derives a stable operator Ed25519 keypair from a testnet node key.
pub fn derive_operator_keypair(node_key: &str) -> anyhow::Result<(String, SigningKey)> {
    if node_key.is_empty() {
        anyhow::bail!("node key is required");
    }
    let seed = hkdf_sha256(node_key.as_bytes(), &[], HKDF_INFO, 32);
    let seed_array: [u8; 32] = seed
        .try_into()
        .map_err(|_| anyhow::anyhow!("hkdf seed must be 32 bytes"))?;
    let signing_key = SigningKey::from_bytes(&seed_array);
    let operator_id = operator_id_from_pubkey(signing_key.verifying_key());
    Ok((operator_id, signing_key))
}

pub fn operator_id_from_pubkey(pubkey: VerifyingKey) -> String {
    let bytes = pubkey.to_bytes();
    hex::encode(Sha256::digest(bytes))
}

pub fn serialize_operator_bid_payload(
    operator_id: &str,
    node_id: &str,
    task_id: &str,
    stake_amount: &num_bigint::BigInt,
) -> Vec<u8> {
    let stake = stake_amount.to_string();
    let mut payload = Vec::with_capacity(
        operator_id.len() + node_id.len() + task_id.len() + stake.len(),
    );
    payload.extend_from_slice(operator_id.as_bytes());
    payload.extend_from_slice(node_id.as_bytes());
    payload.extend_from_slice(task_id.as_bytes());
    payload.extend_from_slice(stake.as_bytes());
    payload
}

fn hkdf_sha256(ikm: &[u8], salt: &[u8], info: &[u8], length: usize) -> Vec<u8> {
    let salt = if salt.is_empty() {
        vec![0u8; 32]
    } else {
        salt.to_vec()
    };
    let mut mac = HmacSha256::new_from_slice(&salt).expect("hmac key");
    mac.update(ikm);
    let prk = mac.finalize().into_bytes();
    hkdf_expand(&prk, info, length)
}

fn hkdf_expand(prk: &[u8], info: &[u8], length: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(length);
    let mut t = Vec::new();
    let mut counter: u8 = 1;
    while out.len() < length {
        let mut mac = HmacSha256::new_from_slice(prk).expect("hmac key");
        mac.update(&t);
        mac.update(info);
        mac.update(&[counter]);
        t = mac.finalize().into_bytes().to_vec();
        out.extend_from_slice(&t);
        counter = counter.saturating_add(1);
    }
    out.truncate(length);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use ed25519_dalek::Signer;

    #[test]
    fn derive_operator_keypair_golden_vector() {
        let (operator_id, key) =
            derive_operator_keypair("nk_crosslang_test_vector").unwrap();
        assert_eq!(
            operator_id,
            "ffd5bedd67236d70d73de1ebb588fe1701068af1eb7f7754e3fcc850df70d01b"
        );
        assert_eq!(
            STANDARD.encode(key.verifying_key().to_bytes()),
            "1jdCe6/5SZ88Zx+JPWFSzZIzRxrA1yx3F01PREE+FMs="
        );
    }

    #[test]
    fn derive_operator_keypair_is_deterministic() {
        let (id1, key1) = derive_operator_keypair("nk_test_key_alpha").unwrap();
        let (id2, key2) = derive_operator_keypair("nk_test_key_alpha").unwrap();
        assert_eq!(id1, id2);
        assert_eq!(key1.to_bytes(), key2.to_bytes());
    }

    #[test]
    fn operator_bid_signature_round_trip() {
        let (operator_id, key) = derive_operator_keypair("nk_roundtrip_example").unwrap();
        let stake = num_bigint::BigInt::from(50_000_000_000_000_000i64);
        let payload =
            serialize_operator_bid_payload(&operator_id, "12D3KooWTest", "task-1", &stake);
        let sig = key.sign(&payload);
        assert!(key
            .verifying_key()
            .verify_strict(&payload, &sig)
            .is_ok());
    }
}
