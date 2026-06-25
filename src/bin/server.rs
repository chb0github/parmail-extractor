use anyhow::Result;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{error, info};
use uuid::Uuid;

use extractor::storage::{InMemoryStore, ManifestStore};

/// Server state
#[derive(Clone)]
struct AppState {
    store: Arc<dyn ManifestStore>,
}

/// Health check response
#[derive(Debug, Serialize)]
struct HealthResponse {
    status: String,
    version: String,
}

/// Error response
#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
    code: u16,
}

/// Extract request payload
#[derive(Debug, Deserialize)]
struct ExtractRequest {
    email_content: String,
    model_id: Option<String>,
}

/// Extract response
#[derive(Debug, Serialize)]
struct ExtractResponse {
    job_id: String,
    status: String,
    message: String,
}

/// Job result response
#[derive(Debug, Serialize)]
struct JobResultResponse {
    job_id: String,
    status: String,
    manifest: Option<serde_json::Value>,
    error: Option<String>,
}

/// Health check endpoint
async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
        version: "0.1.0".to_string(),
    })
}

/// POST /api/extract - Submit email for extraction
async fn extract(
    State(state): State<AppState>,
    Json(payload): Json<ExtractRequest>,
) -> Result<(StatusCode, Json<ExtractResponse>), ApiError> {
    // Validate input
    if payload.email_content.is_empty() {
        return Err(ApiError::BadRequest("email_content is required".to_string()));
    }

    let job_id = Uuid::new_v4().to_string();
    let user_id = "anonymous"; // TODO: Extract from auth header
    let model_id = payload.model_id.unwrap_or_else(|| {
        "us.anthropic.claude-haiku-4-5-20251001-v1:0".to_string()
    });

    info!(
        job_id = %job_id,
        user_id = user_id,
        model_id = model_id,
        "Received extraction request"
    );

    // For now, just create a placeholder manifest
    let manifest = serde_json::json!({
        "id": job_id,
        "model_id": model_id,
        "processed_at": chrono::Utc::now().to_rfc3339(),
        "status": "queued",
        "mail_pieces": []
    });

    state
        .store
        .save(user_id, &job_id, &manifest.to_string())
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to save job: {}", e)))?;

    Ok((
        StatusCode::ACCEPTED,
        Json(ExtractResponse {
            job_id,
            status: "queued".to_string(),
            message: "Email submitted for extraction".to_string(),
        }),
    ))
}

/// GET /api/results/{job_id} - Retrieve extraction results
async fn get_result(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> Result<Json<JobResultResponse>, ApiError> {
    let user_id = "anonymous"; // TODO: Extract from auth header

    let manifest_json = state
        .store
        .get(user_id, &job_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to retrieve job: {}", e)))?;

    match manifest_json {
        Some(json_str) => {
            let manifest: serde_json::Value = serde_json::from_str(&json_str)
                .map_err(|e| ApiError::Internal(format!("Invalid manifest JSON: {}", e)))?;
            Ok(Json(JobResultResponse {
                job_id,
                status: "success".to_string(),
                manifest: Some(manifest),
                error: None,
            }))
        }
        None => Err(ApiError::NotFound("Job not found".to_string())),
    }
}

/// Custom error type for API responses
enum ApiError {
    BadRequest(String),
    NotFound(String),
    Unauthorized(String),
    RateLimit(String),
    Internal(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            ApiError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg),
            ApiError::NotFound(msg) => (StatusCode::NOT_FOUND, msg),
            ApiError::Unauthorized(msg) => (StatusCode::UNAUTHORIZED, msg),
            ApiError::RateLimit(msg) => (StatusCode::TOO_MANY_REQUESTS, msg),
            ApiError::Internal(msg) => {
                error!("Internal error: {}", msg);
                (StatusCode::INTERNAL_SERVER_ERROR, msg)
            }
        };

        let error_response = ErrorResponse {
            error: message,
            code: status.as_u16(),
        };

        (status, Json(error_response)).into_response()
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter("info,extractor=debug")
        .init();

    info!("Starting Extractor Server v0.1.0");

    // Initialize storage
    let store = Arc::new(InMemoryStore::new());

    let app_state = AppState { store };

    // Build router
    let app = Router::new()
        .route("/health", get(health))
        .route("/api/extract", post(extract))
        .route("/api/results/:job_id", get(get_result))
        .with_state(app_state);

    // Run server
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8000").await?;
    info!("Server listening on 0.0.0.0:8000");

    axum::serve(listener, app).await?;

    Ok(())
}
