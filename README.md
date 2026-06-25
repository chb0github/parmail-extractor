# Parmail Extractor Webservice

A RESTful HTTP service for extracting, analyzing, and classifying USPS Informed Delivery emails.

## Overview

**Extractor** is the HTTP-based interface to the email extraction pipeline. It provides:

- **Async email processing** via AWS Bedrock (Claude models)
- **Mail classification** (advertising, financial, personal, etc.)
- **Address extraction** (sender and recipient)
- **Image extraction** (mailer front/back)
- **Job-based architecture** for long-running analysis

## Quick Start

### Prerequisites

- Docker & Docker Compose
- Rust 1.82+ (for local development)
- AWS credentials (for Bedrock access)

### Local Development

```bash
# Build
PATH="/usr/bin:/bin:$PATH" cargo build --release

# Run HTTP server (listens on :8000)
./target/release/extractor

# In another terminal, test endpoints
curl -s http://localhost:8000/health | jq .
```

### Docker Compose

```bash
docker-compose up
```

Includes:
- **Extractor** service (port 8000)
- **PostgreSQL** (port 5432) — manifest storage
- **Redis** (port 6379) — job queue
- **LocalStack** (port 4566) — local S3 for testing

## API Reference

### GET /health

Health check endpoint.

**Response (200):**
```json
{
  "status": "ok",
  "version": "0.1.0"
}
```

### POST /api/extract

Submit an email for extraction.

**Headers:**
```
Content-Type: application/json
```

**Body:**
```json
{
  "email_content": "<raw RFC 2822 email>",
  "model_id": "us.anthropic.claude-haiku-4-5-20251001-v1:0"
}
```

**Response (202 Accepted):**
```json
{
  "job_id": "550e8400-e29b-41d4-a716-446655440000",
  "status": "queued",
  "message": "Email submitted for extraction"
}
```

**Error (400):**
```json
{
  "error": "email_content is required",
  "code": 400
}
```

### GET /api/results/{job_id}

Retrieve extraction results for a submitted job.

**Response (200):**
```json
{
  "job_id": "550e8400-e29b-41d4-a716-446655440000",
  "status": "success",
  "manifest": {
    "id": "550e8400-e29b-41d4-a716-446655440000",
    "model_id": "us.anthropic.claude-haiku-4-5-20251001-v1:0",
    "processed_at": "2026-06-24T10:30:00Z",
    "mail_pieces": [
      {
        "id": "046a09c35047f728",
        "from_address": {
          "name": "Some Business",
          "street": "123 Main St",
          "city": "Anytown",
          "state": "CA",
          "zip": "90210",
          "resolved": true
        },
        "to_address": null,
        "mail_type": "advertising",
        "confidence": 0.95
      }
    ]
  },
  "error": null
}
```

**Error (404):**
```json
{
  "error": "Job not found",
  "code": 404
}
```

## Environment Variables

- `RUST_LOG` — Log level (default: `info`)
- `AWS_REGION` — AWS region (default: `us-west-2`)
- `AWS_ENDPOINT_URL_S3` — S3 endpoint for LocalStack (e.g., `http://localstack:4566`)
- `MANIFEST_STORE` — Storage backend: `memory`, `postgres`, or `s3` (default: `memory`)
- `DATABASE_URL` — PostgreSQL connection string (if using postgres store)

## Architecture

### Storage Abstraction

The server uses a pluggable storage trait `ManifestStore` for flexibility:

```rust
#[async_trait]
pub trait ManifestStore: Send + Sync {
    async fn save(&self, user_id: &str, job_id: &str, manifest_json: &str) -> Result<String>;
    async fn get(&self, user_id: &str, job_id: &str) -> Result<Option<String>>;
    async fn list(&self, user_id: &str, limit: u32) -> Result<Vec<ManifestMetadata>>;
    async fn delete(&self, user_id: &str, job_id: &str) -> Result<()>;
}
```

Implementations:
- **InMemoryStore** — Development/testing (volatile)
- **PostgresStore** — Production (persistent)
- **S3Store** — Archive (JSON blobs in S3)

### Processing Pipeline

```
HTTP Request
    ↓
parse email (mail-parser crate)
    ↓
extract images
    ↓
invoke AWS Bedrock (Claude model)
    ↓
parse response
    ↓
save manifest to store
    ↓
return job_id to client
```

## Development Workflows

### Adding a New Storage Implementation

1. Implement `ManifestStore` trait in `src/storage/postgres.rs` (or similar)
2. Add feature flag to `Cargo.toml`
3. Conditionally initialize in `main()`

Example:
```rust
#[async_trait]
impl ManifestStore for PostgresStore {
    async fn save(&self, user_id: &str, job_id: &str, manifest_json: &str) -> Result<String> {
        // INSERT into database
    }
    // ... other methods
}
```

### Testing Endpoints Locally

```bash
# Health check
curl http://localhost:8000/health

# Submit extraction
curl -X POST http://localhost:8000/api/extract \
  -H "Content-Type: application/json" \
  -d '{"email_content":"From: test@example.com\n\nBody"}'

# Retrieve results (replace JOB_ID)
curl http://localhost:8000/api/results/{JOB_ID}
```

## Known Limitations

- **In-Memory Store:** Data lost on restart; development/testing only
- **Recipient Address Extraction:** See [Known Issues in Parmail](../CLAUDE.md#recipient-address-extraction-issue)
- **No authentication:** All endpoints are public (TODO: add API key support)
- **Blocking Bedrock calls:** Consider async job queue for production scale

## Roadmap

- [ ] PostgreSQL storage implementation
- [ ] Redis job queue + worker pool
- [ ] Async background processing (move Bedrock calls off HTTP path)
- [ ] API key authentication
- [ ] Request rate limiting
- [ ] WebSocket subscription to job status updates
- [ ] Batch upload (multipart email archives)
- [ ] Webhook callbacks on job completion

## Testing

```bash
# Run tests
PATH="/usr/bin:/bin:$PATH" cargo test

# With logging
RUST_LOG=debug cargo test -- --nocapture
```

## License

Proprietary — Parmail Project
