use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// API key record stored on server - contains hash, not the actual key
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKeyRecord {
    /// Unique identifier for this key
    pub id: String,
    /// Prefix for key lookup (first part of key before the secret)
    pub prefix: String,
    /// HMAC-SHA256 hash of full key: HMAC(server_pepper, prefix:secret)
    pub hash: String,
    /// When this key was created (Unix timestamp in milliseconds)
    pub created_at: u64,
    /// When this key was disabled (None if still active)
    pub disabled_at: Option<u64>,
    /// Human-readable label for this key
    pub label: Option<String>,
}

/// API key verifier - handles authentication using hashed records
#[derive(Clone)]
pub struct ApiKeyVerifier {
    /// Map from prefix to full record
    records: Arc<HashMap<String, ApiKeyRecord>>,
    /// Server pepper for HMAC - should be stored securely
    pepper: String,
}

impl ApiKeyVerifier {
    /// Load API key records from JSON file
    pub fn load_from_file(path: &std::path::Path, pepper: &str) -> Result<Self> {
        let data = std::fs::read_to_string(path)?;
        let records: Vec<ApiKeyRecord> = serde_json::from_str(&data)?;

        let mut map = HashMap::new();
        for record in records {
            if record.disabled_at.is_none() {
                map.insert(record.prefix.clone(), record);
            }
        }

        Ok(Self {
            records: Arc::new(map),
            pepper: pepper.to_string(),
        })
    }

    /// Verify a bearer token against stored records.
    /// Token format: "prefix.secret"
    /// Returns the key record on success, Err on failure.
    pub fn verify(&self, token: &str) -> Result<ApiKeyRecord> {
        // Split token into prefix.secret
        let parts: Vec<&str> = token.splitn(2, '.').collect();
        if parts.len() != 2 {
            bail!("invalid token format, expected prefix.secret");
        }

        let (prefix, secret) = (parts[0], parts[1]);

        // Lookup record by prefix
        let record = match self.records.get(prefix) {
            Some(r) => r,
            None => bail!("key not found"),
        };

        // Check if key is disabled
        if record.disabled_at.is_some() {
            bail!("key is disabled");
        }

        // Compute expected hash: HMAC-SHA256(pepper, prefix:secret)
        let mut mac = hmac_sha256::HMAC::new(self.pepper.as_bytes());
        mac.update(format!("{}:{}", prefix, secret).as_bytes());
        let result = mac.finalize();
        let computed_hash = hex::encode(result);

        // Constant-time comparison
        if computed_hash == record.hash {
            Ok(record.clone())
        } else {
            bail!("invalid key");
        }
    }

    /// Check if verifier has no active keys
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Get the number of active keys
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Get key info (without sensitive data) for auditing
    #[allow(dead_code)]
    pub fn get_key_info(&self, prefix: &str) -> Option<KeyInfo> {
        self.records.get(prefix).map(|r| KeyInfo {
            id: r.id.clone(),
            prefix: r.prefix.clone(),
            created_at: r.created_at,
            disabled_at: r.disabled_at,
            label: r.label.clone(),
        })
    }
}

/// Non-sensitive key information for auditing
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct KeyInfo {
    pub id: String,
    pub prefix: String,
    pub created_at: u64,
    pub disabled_at: Option<u64>,
    pub label: Option<String>,
}

/// Generate API key and verifier record together
#[allow(dead_code)]
pub fn generate_api_key(label: &str, pepper: &str) -> (String, ApiKeyRecord) {
    use std::time::{SystemTime, UNIX_EPOCH};

    let id = format!(
        "key_{}",
        uuid::Uuid::new_v4().to_string().split('-').next().unwrap()
    );
    let prefix = format!("zb_{}", &uuid::Uuid::new_v4().to_string()[..8]);
    let secret = uuid::Uuid::new_v4().to_string();
    let token = format!("{}.{}", prefix, secret);
    let created_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    // Compute hash: HMAC-SHA256(pepper, prefix:secret)
    let mut mac = hmac_sha256::HMAC::new(pepper.as_bytes());
    mac.update(format!("{}:{}", prefix, secret).as_bytes());
    let result = mac.finalize();
    let hash = hex::encode(result);

    let record = ApiKeyRecord {
        id,
        prefix: prefix.clone(),
        hash,
        created_at,
        disabled_at: None,
        label: Some(label.to_string()),
    };

    (token, record)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_verify_valid_key() {
        let pepper = "test-pepper";
        let (token, record) = generate_api_key("test-key", pepper);

        let mut map = HashMap::new();
        map.insert(record.prefix.clone(), record);

        let verifier = ApiKeyVerifier {
            records: Arc::new(map),
            pepper: pepper.to_string(),
        };

        assert!(verifier.verify(&token).is_ok());
    }

    #[test]
    fn test_verify_invalid_key() {
        let pepper = "test-pepper";
        let (_, record) = generate_api_key("test-key", pepper);

        let mut map = HashMap::new();
        map.insert(record.prefix.clone(), record);

        let verifier = ApiKeyVerifier {
            records: Arc::new(map),
            pepper: pepper.to_string(),
        };

        assert!(verifier.verify("invalid.token").is_err());
    }

    #[test]
    fn test_verify_wrong_secret() {
        let pepper = "test-pepper";
        let (token, record) = generate_api_key("test-key", pepper);

        let mut map = HashMap::new();
        map.insert(record.prefix.clone(), record);

        let verifier = ApiKeyVerifier {
            records: Arc::new(map),
            pepper: pepper.to_string(),
        };

        // Change the secret part
        let parts: Vec<&str> = token.split('.').collect();
        let wrong_token = format!("{}.{}", parts[0], "wrong-secret");

        assert!(verifier.verify(&wrong_token).is_err());
    }
}
