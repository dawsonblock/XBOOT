use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

// Signing-related functionality for template manifest verification.
// This module provides Ed25519 signature verification for production templates
// against a trusted key ring.

/// Signature algorithm identifier
pub const SIG_ALGORITHM_ED25519: &str = "ed25519";
pub const REQUIRED_MANIFEST_SIGNED_FIELDS: &[&str] = &[
    "artifact_set_id",
    "build_id",
    "created_at_unix_ms",
    "firecracker_binary_sha256",
    "firecracker_version",
    "init_path",
    "kernel_path",
    "kernel_sha256",
    "language",
    "mem_size_mib",
    "promotion_channel",
    "protocol_version",
    "rootfs_path",
    "rootfs_sha256",
    "schema_version",
    "snapshot_mem_bytes",
    "snapshot_mem_path",
    "snapshot_mem_sha256",
    "snapshot_state_bytes",
    "snapshot_state_path",
    "snapshot_state_sha256",
    "template_id",
    "vcpu_count",
];

/// Trusted signing key
#[derive(Debug, Clone)]
pub struct TrustedKey {
    pub key_id: String,
    pub algorithm: String,
    pub public_key: Vec<u8>,
    pub enabled: bool,
    #[allow(dead_code)]
    pub description: Option<String>,
}

/// Keyring containing trusted signing keys
#[derive(Debug, Clone)]
pub struct Keyring {
    keys: Arc<HashMap<String, TrustedKey>>,
}

impl Keyring {
    /// Create a new empty keyring
    pub fn new() -> Self {
        Self {
            keys: Arc::new(HashMap::new()),
        }
    }

    /// Create keyring from a collection of keys
    pub fn from_keys(keys: Vec<TrustedKey>) -> Self {
        let mut map = HashMap::new();
        for key in keys {
            map.insert(key.key_id.clone(), key);
        }
        Self {
            keys: Arc::new(map),
        }
    }

    /// Get a key by ID
    pub fn get(&self, key_id: &str) -> Option<&TrustedKey> {
        self.keys.get(key_id)
    }

    /// Check if a key exists and is enabled
    #[allow(dead_code)]
    pub fn is_trusted(&self, key_id: &str) -> bool {
        self.keys.get(key_id).map(|k| k.enabled).unwrap_or(false)
    }

    /// Get all enabled key IDs
    #[allow(dead_code)]
    pub fn trusted_key_ids(&self) -> Vec<String> {
        self.keys
            .values()
            .filter(|k| k.enabled)
            .map(|k| k.key_id.clone())
            .collect()
    }
}

impl Default for Keyring {
    fn default() -> Self {
        Self::new()
    }
}

/// Load trusted signing keys from a keyring file
///
/// Keyring file format (JSON):
/// ```json
/// {
///   "keys": [
///     {
///       "key_id": "prod-signer-001",
///       "algorithm": "ed25519",
///       "public_key": "base64-encoded-public-key",
///       "enabled": true,
///       "description": "Production signing key"
///     }
///   ]
/// }
/// ```
pub fn load_keyring(path: &Path) -> Result<Keyring> {
    let content = std::fs::read_to_string(path)?;
    let data: serde_json::Value = serde_json::from_str(&content).context("invalid keyring JSON")?;

    let keys_array = data
        .get("keys")
        .context("missing 'keys' field in keyring")?
        .as_array()
        .context("'keys' must be an array")?;

    let mut keys = Vec::new();
    for (i, key_obj) in keys_array.iter().enumerate() {
        let key_id = key_obj
            .get("key_id")
            .context(format!("key[{}]: missing 'key_id'", i))?
            .as_str()
            .context(format!("key[{}]: 'key_id' must be string", i))?
            .to_string();

        let algorithm = key_obj
            .get("algorithm")
            .and_then(|v| v.as_str())
            .unwrap_or(SIG_ALGORITHM_ED25519)
            .to_string();

        let public_key_b64 = key_obj
            .get("public_key")
            .context(format!("key[{}]: missing 'public_key'", i))?
            .as_str()
            .context(format!("key[{}]: 'public_key' must be string", i))?;

        let public_key =
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, public_key_b64)
                .context(format!("key[{}]: invalid base64 in 'public_key'", i))?;

        let enabled = key_obj
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let description = key_obj
            .get("description")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        keys.push(TrustedKey {
            key_id,
            algorithm,
            public_key,
            enabled,
            description,
        });
    }

    Ok(Keyring::from_keys(keys))
}

/// Verify a manifest signature against a trusted signer
///
/// The manifest must contain:
/// - signer_key_id: ID of the signing key
/// - manifest_signature: Base64-encoded signature
/// - manifest_signed_fields: List of field names that were signed
///
/// The signature is computed over the canonicalized JSON of signed fields.
pub fn verify_manifest_signature(
    manifest_json: &str,
    signer_key_id: &str,
    signature_b64: &str,
    keyring: Option<&Keyring>,
) -> Result<bool> {
    // Parse the manifest to extract signature info
    let manifest: serde_json::Value =
        serde_json::from_str(manifest_json).context("invalid manifest JSON")?;

    // Get the fields that were signed
    let signed_fields = manifest
        .get("manifest_signed_fields")
        .and_then(|v| v.as_array())
        .context("missing manifest_signed_fields")?
        .iter()
        .filter_map(|v| v.as_str())
        .collect::<Vec<_>>();

    if signed_fields.is_empty() {
        bail!("manifest_signed_fields is empty - nothing to verify");
    }

    validate_manifest_signed_fields(&signed_fields)?;

    // Use canonical payload for verification
    let payload = canonical_manifest_payload(&manifest, &signed_fields)?;

    // Decode the signature
    let signature_bytes =
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, signature_b64)
            .context("invalid base64 signature")?;

    // Verify against keyring
    if let Some(keyring) = keyring {
        let key = keyring.get(signer_key_id).context(format!(
            "signer key '{}' not found in keyring",
            signer_key_id
        ))?;

        if !key.enabled {
            bail!("signer key '{}' is disabled", signer_key_id);
        }

        match key.algorithm.as_str() {
            SIG_ALGORITHM_ED25519 => {
                if key.public_key.len() != 32 {
                    bail!(
                        "invalid Ed25519 public key length: {}",
                        key.public_key.len()
                    );
                }

                // Use ring or ed25519-dalek for verification
                // For now, use ring crate
                use ring::signature::{UnparsedPublicKey, ED25519};

                let public_key = UnparsedPublicKey::new(&ED25519, &key.public_key);
                public_key
                    .verify(&payload, &signature_bytes)
                    .map_err(|e| anyhow::anyhow!("signature verification failed: {:?}", e))?;
            }
            _ => {
                bail!("unsupported signature algorithm: {}", key.algorithm);
            }
        }

        Ok(true)
    } else {
        // No keyring provided - check if we should allow unverified
        bail!("no keyring provided for signature verification");
    }
}

/// Verify manifest without a loaded keyring (legacy compatibility)
///
/// This returns an error prompting the user to configure signature verification.
#[allow(dead_code)]
pub fn verify_manifest_signature_stub(
    _manifest_json: &str,
    _signer_key_id: &str,
    _signature: &str,
) -> Result<bool> {
    bail!("signature verification requires keyring - set ZEROBOOT_REQUIRE_TEMPLATE_SIGNATURES=0 to skip or configure keyring path");
}

/// Generate a new signing key pair (for key rotation)
pub fn generate_key_pair() -> Result<(Vec<u8>, Vec<u8>)> {
    use ring::signature::{Ed25519KeyPair, KeyPair};

    let rng = ring::rand::SystemRandom::new();
    let pkcs8_bytes = Ed25519KeyPair::generate_pkcs8(&rng)
        .map_err(|e| anyhow::anyhow!("failed to generate key pair: {:?}", e))?;

    // The pkcs8_bytes contains both private and public key
    // We can extract the public key from it by parsing
    let key_pair = Ed25519KeyPair::from_pkcs8(pkcs8_bytes.as_ref())
        .map_err(|e| anyhow::anyhow!("failed to parse generated key: {:?}", e))?;

    // Return pkcs8 bytes (contains both keys) and public key separately
    let public_key = key_pair.public_key().as_ref().to_vec();
    Ok((pkcs8_bytes.as_ref().to_vec(), public_key))
}

/// Export public key from a key pair
pub fn export_public_key(key_pair_bytes: &[u8]) -> Result<Vec<u8>> {
    use ring::signature::{Ed25519KeyPair, KeyPair};

    let key_pair = Ed25519KeyPair::from_pkcs8(key_pair_bytes)
        .map_err(|e| anyhow::anyhow!("invalid key pair: {:?}", e))?;

    Ok(key_pair.public_key().as_ref().to_vec())
}

/// Sign a manifest and return the signature
///
/// Takes the private key (pkcs8 format) and manifest JSON,
/// signs it with Ed25519, and returns the signature as base64.
pub fn sign_manifest(
    key_pair_bytes: &[u8],
    manifest_json: &str,
    signed_fields: &[&str],
) -> Result<(String, String)> {
    use ring::signature::Ed25519KeyPair;

    let key_pair = Ed25519KeyPair::from_pkcs8(key_pair_bytes)
        .map_err(|e| anyhow::anyhow!("invalid key pair: {:?}", e))?;

    // Parse manifest to get JSON value
    let manifest: serde_json::Value =
        serde_json::from_str(manifest_json).context("invalid manifest JSON")?;

    // Use canonical payload for deterministic signing
    let payload = canonical_manifest_payload(&manifest, signed_fields)?;

    let signature = key_pair.sign(&payload);
    let signature_b64 = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        signature.as_ref(),
    );

    Ok((signature_b64, String::from_utf8_lossy(&payload).to_string()))
}

pub fn sign_manifest_with_required_fields(
    key_pair_bytes: &[u8],
    manifest_json: &str,
) -> Result<(String, String)> {
    sign_manifest(
        key_pair_bytes,
        manifest_json,
        REQUIRED_MANIFEST_SIGNED_FIELDS,
    )
}

/// Get the public key ID from a key pair (SHA256 hash of public key)
pub fn get_key_id(public_key: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(public_key);
    hex::encode(hasher.finalize())[..16].to_string()
}

/// Sign data with a key pair
#[allow(dead_code)]
pub fn sign_data(key_pair_bytes: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    use ring::signature::Ed25519KeyPair;

    let key_pair = Ed25519KeyPair::from_pkcs8(key_pair_bytes)
        .map_err(|e| anyhow::anyhow!("invalid key pair: {:?}", e))?;

    Ok(key_pair.sign(data).as_ref().to_vec())
}

/// Create a canonical manifest payload for signing.
///
/// This function:
/// - Sorts the signed_fields lexicographically
/// - Builds a deterministic text payload: `field_name=<canonical-json>\n`
/// - Rejects empty field list and duplicates
///
/// This ensures signatures are unambiguous regardless of field ordering in the manifest.
pub fn canonical_manifest_payload(
    manifest: &serde_json::Value,
    signed_fields: &[&str],
) -> Result<Vec<u8>> {
    if signed_fields.is_empty() {
        bail!("cannot create canonical payload: signed_fields is empty");
    }

    // Check for duplicates
    let mut seen = std::collections::HashSet::new();
    for field in signed_fields {
        if !seen.insert(field) {
            bail!(
                "cannot create canonical payload: duplicate field '{}'",
                field
            );
        }
    }

    // Sort fields lexicographically
    let mut sorted_fields: Vec<&str> = signed_fields.to_vec();
    sorted_fields.sort();

    // Build deterministic payload: field_name=<canonical-json>\n
    let mut payload = String::new();
    for field in sorted_fields {
        let value = manifest.get(field).ok_or_else(|| {
            anyhow::anyhow!("canonical payload: manifest missing field '{}'", field)
        })?;
        let canonical = serde_json::to_string(value)?;
        payload.push_str(field);
        payload.push('=');
        payload.push_str(&canonical);
        payload.push('\n');
    }

    Ok(payload.into_bytes())
}

pub fn required_manifest_signed_fields_vec() -> Vec<String> {
    REQUIRED_MANIFEST_SIGNED_FIELDS
        .iter()
        .map(|field| (*field).to_string())
        .collect()
}

pub fn validate_manifest_signed_fields<S>(signed_fields: &[S]) -> Result<()>
where
    S: AsRef<str>,
{
    if signed_fields.is_empty() {
        bail!("manifest_signed_fields is empty");
    }

    let mut actual = signed_fields
        .iter()
        .map(|field| field.as_ref().to_string())
        .collect::<Vec<_>>();
    let actual_len = actual.len();
    actual.sort();
    actual.dedup();
    if actual.len() != actual_len {
        bail!("manifest_signed_fields contains duplicates");
    }

    let mut expected = REQUIRED_MANIFEST_SIGNED_FIELDS
        .iter()
        .map(|field| (*field).to_string())
        .collect::<Vec<_>>();
    expected.sort();

    if actual != expected {
        bail!(
            "manifest_signed_fields must exactly match the required signing set: {:?}",
            REQUIRED_MANIFEST_SIGNED_FIELDS
        );
    }

    Ok(())
}

/// Format public key for keyring (base64)
pub fn format_public_key_base64(public_key: &[u8]) -> String {
    base64::Engine::encode(&base64::engine::general_purpose::STANDARD, public_key)
}

/// Parse public key from base64
#[allow(dead_code)]
pub fn parse_public_key_base64(b64: &str) -> Result<Vec<u8>> {
    base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64)
        .context("invalid base64 public key")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keyring_basics() {
        let key = TrustedKey {
            key_id: "test-key".to_string(),
            algorithm: SIG_ALGORITHM_ED25519.to_string(),
            public_key: vec![0u8; 32],
            enabled: true,
            description: Some("Test key".to_string()),
        };

        let keyring = Keyring::from_keys(vec![key]);

        assert!(keyring.is_trusted("test-key"));
        assert!(!keyring.is_trusted("nonexistent"));
        assert_eq!(keyring.trusted_key_ids(), vec!["test-key"]);
    }

    #[test]
    fn test_keyring_disabled_key() {
        let key = TrustedKey {
            key_id: "disabled-key".to_string(),
            algorithm: SIG_ALGORITHM_ED25519.to_string(),
            public_key: vec![0u8; 32],
            enabled: false,
            description: None,
        };

        let keyring = Keyring::from_keys(vec![key]);

        assert!(!keyring.is_trusted("disabled-key"));
    }

    #[test]
    fn test_format_public_key_base64() {
        let pk = vec![0u8; 32];
        let b64 = format_public_key_base64(&pk);
        assert_eq!(b64.len(), 44); // 32 bytes base64 encoded = 44 chars with padding

        let parsed = parse_public_key_base64(&b64).unwrap();
        assert_eq!(parsed, pk);
    }

    #[test]
    fn test_canonical_payload_empty_fields_fails() {
        let manifest = serde_json::json!({"foo": "bar"});
        let result = canonical_manifest_payload(&manifest, &[]);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("empty"));
    }

    #[test]
    fn test_canonical_payload_duplicate_fields_fails() {
        let manifest = serde_json::json!({"foo": "bar"});
        let result = canonical_manifest_payload(&manifest, &["foo", "foo"]);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("duplicate"));
    }

    #[test]
    fn test_canonical_payload_missing_field_fails() {
        let manifest = serde_json::json!({"foo": "bar"});
        let result = canonical_manifest_payload(&manifest, &["foo", "baz"]);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("missing"));
    }

    #[test]
    fn test_canonical_payload_deterministic() {
        let manifest = serde_json::json!({
            "template_id": "test-001",
            "build_id": "build-001",
            "artifact_set_id": "art-001"
        });

        // Sign with fields in different order - should produce same payload
        let payload1 =
            canonical_manifest_payload(&manifest, &["template_id", "build_id", "artifact_set_id"])
                .unwrap();
        let payload2 =
            canonical_manifest_payload(&manifest, &["artifact_set_id", "template_id", "build_id"])
                .unwrap();
        let payload3 =
            canonical_manifest_payload(&manifest, &["build_id", "artifact_set_id", "template_id"])
                .unwrap();

        assert_eq!(payload1, payload2);
        assert_eq!(payload2, payload3);

        // Verify the payload format
        let payload_str = String::from_utf8_lossy(&payload1);
        assert!(payload_str.contains("artifact_set_id="));
        assert!(payload_str.contains("build_id="));
        assert!(payload_str.contains("template_id="));
        assert!(payload_str.contains('\n'));
    }

    #[test]
    fn test_canonical_payload_sorted_order() {
        let manifest = serde_json::json!({
            "z_field": "z",
            "a_field": "a",
            "m_field": "m"
        });

        let payload =
            canonical_manifest_payload(&manifest, &["z_field", "a_field", "m_field"]).unwrap();
        let payload_str = String::from_utf8_lossy(&payload);

        // Should be sorted lexicographically
        let a_pos = payload_str.find("a_field").unwrap();
        let m_pos = payload_str.find("m_field").unwrap();
        let z_pos = payload_str.find("z_field").unwrap();
        assert!(a_pos < m_pos);
        assert!(m_pos < z_pos);
    }
}
