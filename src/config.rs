use std::env;

pub struct Config {
    /// AWS Bedrock model ID to use for extraction
    pub model_id: String,
    /// Storage backend: "memory", "postgres", or "s3"
    pub storage_backend: String,
    /// PostgreSQL connection string (if storage_backend == "postgres")
    pub postgres_url: String,
    /// Redis URL (for job queue and caching)
    pub redis_url: String,
    /// AWS region
    pub aws_region: String,
    /// AWS S3 bucket for storing manifests
    pub s3_bucket: String,
    /// AWS S3 bucket prefix for output
    pub s3_prefix: String,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            model_id: env::var("MODEL_ID")
                .unwrap_or_else(|_| "us.anthropic.claude-haiku-4-5-20251001-v1:0".to_string()),
            storage_backend: env::var("STORAGE_BACKEND")
                .unwrap_or_else(|_| "memory".to_string()),
            postgres_url: env::var("POSTGRES_URL")
                .unwrap_or_else(|_| "postgresql://localhost/parmail".to_string()),
            redis_url: env::var("REDIS_URL")
                .unwrap_or_else(|_| "redis://localhost:6379".to_string()),
            aws_region: env::var("AWS_REGION")
                .unwrap_or_else(|_| "us-west-2".to_string()),
            s3_bucket: env::var("S3_BUCKET")
                .unwrap_or_else(|_| "parmail-output".to_string()),
            s3_prefix: env::var("S3_PREFIX")
                .unwrap_or_else(|_| "output/".to_string()),
        }
    }
}
