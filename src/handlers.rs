use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use chrono::Utc;
use serde_json::json;
use std::sync::Arc;
use tracing::{info, warn, error};
use uuid::Uuid;

use crate::{
    models::{ExtractRequest, ExtractResponse, JobResult, JobStatus},
    AppState,
};

/// POST /api/extract - Submit an email for extraction
pub async fn extract(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<ExtractRequest>,
) -> Result<(StatusCode, Json<ExtractResponse>), (StatusCode, String)> {
    info!("Received extract request");

    // Generate job ID
    let job_id = Uuid::new_v4().to_string();
    info!("Created job_id: {}", job_id);

    // Validate email payload
    if payload.email.is_empty() {
        warn!("Empty email payload");
        return Err((
            StatusCode::BAD_REQUEST,
            "email payload cannot be empty".to_string(),
        ));
    }

    // Create initial job record
    let job = JobResult {
        job_id: job_id.clone(),
        status: JobStatus::Queued,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        manifest: None,
        error: None,
        usage: None,
    };

    if let Err(e) = state.storage.save_job(&job_id, &job).await {
        error!("Failed to save job: {}", e);
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to create job".to_string(),
        ));
    }

    let response = ExtractResponse {
        job_id: job_id.clone(),
        status_url: format!("/api/results/{}", job_id),
    };

    Ok((StatusCode::ACCEPTED, Json(response)))
}

/// GET /api/results/{job_id} - Poll job status and retrieve results
pub async fn get_results(
    State(state): State<Arc<AppState>>,
    Path(job_id): Path<String>,
) -> Result<Json<JobResult>, (StatusCode, String)> {
    info!("Fetching results for job_id: {}", job_id);

    match state.storage.get_job(&job_id).await {
        Ok(Some(job)) => Ok(Json(job)),
        Ok(None) => {
            warn!("Job not found: {}", job_id);
            Err((
                StatusCode::NOT_FOUND,
                format!("Job {} not found", job_id),
            ))
        }
        Err(e) => {
            error!("Failed to retrieve job {}: {}", job_id, e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to retrieve job".to_string(),
            ))
        }
    }
}

/// GET /health - Health check endpoint
pub async fn health() -> impl IntoResponse {
    Json(json!({
        "status": "healthy",
        "timestamp": Utc::now().to_rfc3339()
    }))
}
