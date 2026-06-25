use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{info, error};
use uuid::Uuid;

mod handlers;
mod models;
mod storage;
mod config;

use config::Config;
use models::{ExtractRequest, ExtractResponse, JobStatus};
use storage::StorageBackend;

pub struct AppState {
    storage: Arc<dyn StorageBackend>,
    config: Config,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("parmail_extractor=info".parse()?),
        )
        .init();

    info!("Initializing Parmail Extractor webservice");

    let config = Config::from_env();
    info!("Config loaded: model_id={}, backend={}", config.model_id, config.storage_backend);

    let storage = storage::create_backend(&config).await?;
    let state = Arc::new(AppState {
        storage: Arc::new(storage),
        config,
    });

    let app = Router::new()
        .route("/api/extract", post(handlers::extract))
        .route("/api/results/:job_id", get(handlers::get_results))
        .route("/health", get(handlers::health))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;
    info!("Server listening on 0.0.0.0:3000");

    axum::serve(listener, app).await?;

    Ok(())
}
