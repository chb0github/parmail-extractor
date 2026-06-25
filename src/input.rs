use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::s3::ParmailS3Client;

/// Represents an email with its raw data
pub struct Email {
    data: Vec<u8>,
}

impl Email {
    /// Load email from a path (local file or s3:// URI)
    pub async fn from(source: &str) -> Result<Self> {
        assert!(!source.is_empty(), "source must not be empty");

        let data = match parse_uri(source) {
            Ok(Uri::Local(path)) => tokio::fs::read(&path)
                .await
                .with_context(|| format!("Failed to read {}", path.display()))?,
            Ok(Uri::S3 { bucket, key }) => {
                let client = ParmailS3Client::from_bucket(bucket).await;
                client.get_data(&key).await?
            }
            _ => anyhow::bail!("Invalid URL: {}", source),
        };

        Ok(Self { data })
    }

    /// Get the raw email bytes
    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }
}

#[derive(Debug, Clone)]
pub enum EmailSource {
    Local(PathBuf),
    S3 { bucket: String, key: String },
}

impl std::fmt::Display for EmailSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EmailSource::Local(path) => write!(f, "{}", path.display()),
            EmailSource::S3 { bucket, key } => write!(f, "s3://{bucket}/{key}"),
        }
    }
}

impl EmailSource {
    pub fn short_name(&self) -> String {
        match self {
            EmailSource::Local(path) => path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
            EmailSource::S3 { key, .. } => key.rsplit('/').next().unwrap_or(key).to_string(),
        }
    }
}

pub async fn resolve_sources(paths: &[String]) -> Result<Vec<EmailSource>> {
    let mut sources = Vec::new();

    for p in paths {
        match parse_uri(p) {
            Ok(Uri::S3 {
                bucket,
                key: prefix,
            }) => {
                let client = ParmailS3Client::from_bucket(bucket.clone()).await;
                let keys = client.list_objects(&prefix).await?;
                for key in keys {
                    sources.push(EmailSource::S3 {
                        bucket: bucket.clone(),
                        key,
                    });
                }
            }
            Ok(Uri::Local(path)) => {
                match (path.is_file(), path.is_dir()) {
                    (true, false) => sources.push(EmailSource::Local(path)),
                    (false, true) => walk_dir(&path, &mut sources)?,
                    _ => anyhow::bail!("Path does not exist: {}", path.display()),
                }
            },
            Err(e) => anyhow::bail!("Invalid URI: {}", e),
        }
    }

    sources.sort_by(|a, b| a.to_string().cmp(&b.to_string()));
    sources.dedup_by(|a, b| a.to_string() == b.to_string());
    Ok(sources)
}

pub async fn fetch_email(source: &EmailSource) -> Result<Vec<u8>> {
    match source {
        EmailSource::Local(path) => tokio::fs::read(path)
            .await
            .with_context(|| format!("Failed to read {}", path.display())),
        EmailSource::S3 { bucket, key } => {
            let client = ParmailS3Client::from_bucket(bucket.clone()).await;
            client.get_data(key).await
        }
    }
}

enum Uri {
    S3 { bucket: String, key: String },
    Local(PathBuf),
}

fn parse_uri(input: &str) -> Result<Uri> {
    match input {
        s if s.starts_with("s3://") => {
            let rest = &s[5..];
            let (bucket, key) = match rest.split_once('/') {
                Some((b, k)) => (b.to_string(), k.to_string()),
                None => (rest.to_string(), String::new()),
            };
            Ok(Uri::S3 { bucket, key })
        }
        s if s.starts_with("file://") => Ok(Uri::Local(PathBuf::from(&s[7..]))),
        s if std::fs::exists(s)? => Ok(Uri::Local(PathBuf::from(s))),
        _ => anyhow::bail!("Unrecognized URI: {}", input),
    }
}

fn walk_dir(dir: &Path, sources: &mut Vec<EmailSource>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        match path {
            ref p if p.is_dir() => walk_dir(&path, sources)?,
            _ => sources.push(EmailSource::Local(path)),
        }
    }
    Ok(())
}
