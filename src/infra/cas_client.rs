use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

/// Lowercase hex SHA-256 of `data`.
pub fn sha256_hex(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    hex::encode(digest)
}

/// Returns `Ok(data)` when SHA-256(data) equals `expected_hash` (hex, case-insensitive).
pub fn verify_cas_blob(data: &[u8], expected_hash: &str) -> Result<()> {
    let got = sha256_hex(data);
    if !got.eq_ignore_ascii_case(expected_hash.trim()) {
        bail!(
            "CAS leaf proof hash mismatch: got {} want {}",
            got,
            expected_hash.trim()
        );
    }
    Ok(())
}

/// GET a content-addressed blob from a (presigned) URL and verify its SHA-256.
pub async fn fetch_and_verify(
    client: &reqwest::Client,
    url: &str,
    expected_hash: &str,
    expected_bytes: Option<u64>,
) -> Result<Vec<u8>> {
    // Leaf STARKs can be multi-MiB; allow longer than the geo client's default.
    let response = client
        .get(url)
        .timeout(std::time::Duration::from_secs(120))
        .send()
        .await
        .with_context(|| format!("CAS GET failed for {url}"))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!("CAS GET status {status}: {body}");
    }

    let data = response
        .bytes()
        .await
        .context("CAS GET body read failed")?
        .to_vec();

    if let Some(want) = expected_bytes {
        if want > 0 && data.len() as u64 != want {
            tracing::warn!(
                "[CAS] leaf proof size {} != announced leaf_proof_bytes {}; continuing with hash check",
                data.len(),
                want
            );
        }
    }

    verify_cas_blob(&data, expected_hash)?;
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_cas_blob_accepts_matching_hash() {
        let data = b"leaf-stark-proof-bytes";
        let hash = sha256_hex(data);
        verify_cas_blob(data, &hash).expect("match");
        verify_cas_blob(data, &hash.to_uppercase()).expect("case insensitive");
    }

    #[test]
    fn verify_cas_blob_rejects_mismatch() {
        let err = verify_cas_blob(b"proof-a", &sha256_hex(b"proof-b")).unwrap_err();
        assert!(format!("{err:#}").contains("hash mismatch"), "err={err:#}");
    }
}
