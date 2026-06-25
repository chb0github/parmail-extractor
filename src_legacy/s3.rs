use anyhow::{Context, Result};
use aws_sdk_s3::Client as S3Client;

use crate::models::EmailManifest;

/// Domain-specific S3 client for parmail operations
pub struct ParmailS3Client {
    client: S3Client,
    bucket: String,
}

impl ParmailS3Client {
    pub async fn from_bucket(bucket: String) -> Self {
        let config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
        let client = S3Client::new(&config);
        Self { client, bucket }
    }

    /// List objects with a given prefix
    pub async fn list_objects(&self, prefix: &str) -> Result<Vec<String>> {
        let mut keys = Vec::new();
        let mut continuation_token: Option<String> = None;

        loop {
            let mut req = self.client.list_objects_v2().bucket(&self.bucket);
            if !prefix.is_empty() {
                req = req.prefix(prefix);
            }
            if let Some(ref token) = continuation_token {
                req = req.continuation_token(token);
            }

            let resp = req.send().await
                .with_context(|| format!("Failed to list s3://{}/{}", self.bucket, prefix))?;

            if let Some(ref contents) = resp.contents {
                keys.extend(
                    contents.iter().filter_map(|obj| obj.key()).map(|key| key.to_string())
                );
            }

            match resp.next_continuation_token().map(|t| t.to_string()) {
                Some(next) => continuation_token = Some(next),
                None => break,
            }
        }

        Ok(keys)
    }

    pub async fn list_emails(&self) -> Result<Vec<String>> {
        self.list_objects("emails/").await
    }

    /// Get object data from S3 by key
    /// Returns error if object doesn't exist, otherwise always returns data
    pub async fn get_data(&self, key: &str) -> Result<Vec<u8>> {
        assert!(!key.is_empty(), "key must not be empty");

        let resp = self.client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .with_context(|| format!("Failed to fetch s3://{}/{}", self.bucket, key))?;

        let bytes = resp
            .body
            .collect()
            .await
            .context("Failed to read S3 object body")?
            .into_bytes()
            .to_vec();

        Ok(bytes)
    }

    /// Store processing results to S3
    /// Returns the S3 key root path (e.g., "output/68612578126e984c/")
    pub async fn add_result(
        &self,
        manifest: &EmailManifest,
        piece_id: &str,
        mailer: Option<&[u8]>,
        content: Option<&[u8]>,
    ) -> Result<String> {
        let root = format!("output/{}/", manifest.id);
        let piece_dir = format!("{}{}/", root, piece_id);

        // Store manifest at email root
        let manifest_key = format!("{}manifest.json", root);
        let manifest_json = serde_json::to_string_pretty(manifest)?;
        self.put_object(&manifest_key, manifest_json.as_bytes(), "application/json").await?;

        // Store images in piece subdirectory
        if let Some(data) = mailer {
            let key = format!("{}mailer.jpg", piece_dir);
            self.put_object(&key, data, "image/jpeg").await?;
        }

        if let Some(data) = content {
            let key = format!("{}content.jpg", piece_dir);
            self.put_object(&key, data, "image/jpeg").await?;
        }

        Ok(root)
    }

    async fn put_object(&self, key: &str, data: &[u8], content_type: &str) -> Result<()> {
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .body(data.to_vec().into())
            .content_type(content_type)
            .send()
            .await
            .with_context(|| format!("Failed to put s3://{}/{}", self.bucket, key))?;
        Ok(())
    }
}
