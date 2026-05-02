use crate::AppState;
use axum::body::Bytes;
use axum::http::HeaderMap;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn verify_request_signature(state: &AppState, headers: &HeaderMap, body: &Bytes) -> Result<(), String> {
    // 1. Extract headers
    let pubkey_b64 = read_header(headers, "X-WQC-Orchestrator-PublicKey")?;
    let signature_b64 = read_header(headers, "X-WQC-Signature")?;
    let timestamp_str = read_header(headers, "X-WQC-Timestamp")?;
    let nonce = read_header(headers, "X-WQC-Nonce")?;

    let now = SystemTime::now().duration_since(UNIX_EPOCH)
        .map_err(|_| "System time error")?.as_secs();
    let timestamp: u64 = timestamp_str.parse().map_err(|_| "Invalid timestamp")?;

    // 2. Security Checks (Timestamp & Allowlist)
    if timestamp < now - 300 || timestamp > now + 300 {
        return Err("Timestamp out of window".to_string());
    }

    if !state.allowed_orchestrators.read().unwrap().contains(pubkey_b64) {
        return Err("Unauthorized Public Key".to_string());
    }

    // 3. Replay Protection (Nonce check)
    {
        let mut seen = state.seen_submit_nonces.lock()
            .map_err(|_| "Nonce lock poisoned")?;

        let replay_key = format!("{}:{}", pubkey_b64, nonce);
        let cutoff = now - 300;

        // Cleanup old nonces
        seen.retain(|_, seen_at| *seen_at >= cutoff.try_into().unwrap());

        if seen.contains_key(&replay_key) {
            return Err("Replay Detected (Duplicate Nonce)".to_string());
        }
        seen.insert(replay_key, now.try_into().unwrap());
    }

    // 4. Verify Signature
    let pubkey_bytes = STANDARD.decode(pubkey_b64).map_err(|_| "Invalid pubkey base64")?;
    let verifying_key = VerifyingKey::from_bytes(&pubkey_bytes.try_into().map_err(|_| "Invalid pubkey length")?)
        .map_err(|_| "Invalid verifying key")?;

    let sig_bytes = STANDARD.decode(signature_b64).map_err(|_| "Invalid signature base64")?;
    let signature = Signature::from_bytes(&sig_bytes.try_into().map_err(|_| "Invalid signature length")?);

    let mut hasher = Sha256::new();
    hasher.update(body);
    let body_hash = hex::encode(hasher.finalize());
    let message = format!("WQC-REQUEST-V1\n{}\n{}\n{}", timestamp, nonce, body_hash);

    verifying_key.verify(message.as_bytes(), &signature)
        .map_err(|_| "Invalid Signature".to_string())?;

    Ok(())
}

fn read_header<'a>(headers: &'a HeaderMap, name: &str) -> Result<&'a str, String> {
    headers.get(name).and_then(|v| v.to_str().ok()).ok_or_else(|| format!("Missing header: {}", name))
}

pub fn generate_wqc_headers(
    signing_key: &SigningKey,
    body: &[u8],
    version: &str,
) -> (String, String, String, String) {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .to_string();

    let nonce = uuid::Uuid::new_v4().to_string();
    let pubkey_b64 = STANDARD.encode(signing_key.verifying_key().to_bytes());

    // Message format: version + "\n" + timestamp + "\n" + nonce + "\n" + hex(sha256(body))
    let body_hash = hex::encode(Sha256::digest(body));
    let message = format!("{}\n{}\n{}\n{}", version, timestamp, nonce, body_hash);

    let signature = signing_key.sign(message.as_bytes());
    let signature_b64 = STANDARD.encode(signature.to_bytes());

    (signature_b64, pubkey_b64, nonce, timestamp)
}
