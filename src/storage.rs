use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};

/// Metadata about a stored manifest
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestMetadata {
    pub job_id: String,
    pub user_id: String,
    pub created_at: DateTime<Utc>,
    pub mail_pieces_count: usize,
}

/// Abstraction trait for storing and retrieving manifests
#[async_trait]
pub trait ManifestStore: Send + Sync {
    /// Save a manifest for a user's job
    async fn save(
        &self,
        user_id: &str,
        job_id: &str,
        manifest_json: &str,
    ) -> Result<String>;

    /// Retrieve a manifest for a user's job
    async fn get(&self, user_id: &str, job_id: &str) -> Result<Option<String>>;

    /// List manifests for a user
    async fn list(&self, user_id: &str, limit: u32) -> Result<Vec<ManifestMetadata>>;

    /// Delete a manifest
    async fn delete(&self, user_id: &str, job_id: &str) -> Result<()>;
}

/// In-memory implementation for testing/development
pub struct InMemoryStore {
    data: std::sync::Arc<tokio::sync::RwLock<std::collections::HashMap<String, String>>>,
}

impl InMemoryStore {
    pub fn new() -> Self {
        Self {
            data: std::sync::Arc::new(tokio::sync::RwLock::new(
                std::collections::HashMap::new(),
            )),
        }
    }

    fn make_key(user_id: &str, job_id: &str) -> String {
        format!("{}#{}", user_id, job_id)
    }
}

#[async_trait]
impl ManifestStore for InMemoryStore {
    async fn save(
        &self,
        user_id: &str,
        job_id: &str,
        manifest_json: &str,
    ) -> Result<String> {
        let key = Self::make_key(user_id, job_id);
        let mut data = self.data.write().await;
        data.insert(key.clone(), manifest_json.to_string());
        Ok(key)
    }

    async fn get(&self, user_id: &str, job_id: &str) -> Result<Option<String>> {
        let key = Self::make_key(user_id, job_id);
        let data = self.data.read().await;
        Ok(data.get(&key).cloned())
    }

    async fn list(&self, user_id: &str, limit: u32) -> Result<Vec<ManifestMetadata>> {
        let data = self.data.read().await;
        let prefix = format!("{}#", user_id);
        let mut results = Vec::new();
        for (key, manifest_str) in data.iter() {
            if key.starts_with(&prefix) && (results.len() as u32) < limit {
                let job_id = key.split('#').nth(1).unwrap_or("").to_string();
                // Parse manifest to get piece count
                let piece_count = serde_json::from_str::<serde_json::Value>(manifest_str)
                    .ok()
                    .and_then(|v| v.get("mail_pieces").and_then(|p| p.as_array()).map(|a| a.len()))
                    .unwrap_or(0);

                results.push(ManifestMetadata {
                    job_id,
                    user_id: user_id.to_string(),
                    created_at: Utc::now(),
                    mail_pieces_count: piece_count,
                });
            }
        }
        Ok(results)
    }

    async fn delete(&self, user_id: &str, job_id: &str) -> Result<()> {
        let key = Self::make_key(user_id, job_id);
        let mut data = self.data.write().await;
        data.remove(&key);
        Ok(())
    }
}

// ============================================================================
// Webservice Job Storage Backend (for API jobs)
// ============================================================================

use async_trait::async_trait;
use crate::models::{JobResult, JobStatus};

/// Storage abstraction for job state and results
#[async_trait]
pub trait JobStorageBackend: Send + Sync {
    /// Create or update a job record
    async fn save_job(&self, job_id: &str, result: &JobResult) -> anyhow::Result<()>;

    /// Retrieve a job record by ID
    async fn get_job(&self, job_id: &str) -> anyhow::Result<Option<JobResult>>;

    /// Store extracted manifest JSON
    async fn save_manifest(&self, job_id: &str, manifest: serde_json::Value) -> anyhow::Result<()>;

    /// Store extracted images (mailer.jpg, content.jpg, etc.)
    async fn save_image(&self, job_id: &str, piece_id: &str, image_type: &str, data: bytes::Bytes) -> anyhow::Result<()>;

    /// Retrieve image data
    async fn get_image(&self, job_id: &str, piece_id: &str, image_type: &str) -> anyhow::Result<Option<bytes::Bytes>>;
}
