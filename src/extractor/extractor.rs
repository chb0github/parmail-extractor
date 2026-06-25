use anyhow::Result;
use aws_sdk_bedrockruntime::Client as BedrockClient;
use aws_sdk_s3::Client as S3Client;
use lambda_runtime::{service_fn, LambdaEvent};

use aws_lambda_events::event::sqs::SqsEvent;
use crate::extractor::analysis::ModelConfig;
use crate::extractor::processor::process_raw_email;
use crate::extractor::storage::Storage;
use crate::input;
use crate::models::S3Event;
use crate::ses::EmailError;


pub async fn run_lambda() -> Result<()> {
    let config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
    let s3_client = S3Client::new(&config);
    let bedrock_client = BedrockClient::new(&config);

    let storage_dir =
        std::env::var("STORAGE_DIR").unwrap_or_else(|_| "/tmp/parmail".to_string());

    let model_config = match std::env::var("BEDROCK_MODEL_ID") {
        Ok(id) => {
            let models_file = std::env::var("MODELS_FILE").unwrap_or_else(|_| "models.json".to_string());
            ModelConfig::load(&models_file, &id, false, &storage_dir).unwrap_or_else(|_| ModelConfig::default_config(&storage_dir))
        }
        Err(_) => ModelConfig::default_config(&storage_dir),
    };

    let handler = service_fn(move |event: LambdaEvent<SqsEvent>| {
        let s3 = s3_client.clone();
        let bedrock = bedrock_client.clone();
        let model = model_config.clone();
        let store = Storage::from_uri(&storage_dir, Some(s3.clone()))
            .expect("Invalid STORAGE_DIR");
        async move { handle_sqs_event(&s3, &bedrock, &model, &store, event).await }
    });

    lambda_runtime::run(handler)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(())
}

async fn handle_sqs_event(
    _s3_client: &S3Client,
    bedrock_client: &BedrockClient,
    model: &ModelConfig,
    storage: &Storage,
    event: LambdaEvent<SqsEvent>,
) -> std::result::Result<serde_json::Value, EmailError> {
    let (sqs_event, _context) = event.into_parts();

    for message in &sqs_event.records {
        let body = match &message.body {
            Some(b) => b,
            None => continue,
        };

        let s3_event: S3Event = serde_json::from_str(body)?;

        for record in &s3_event.records {
            let bucket = record.s3.bucket.name.clone();
            let key = record.s3.object.key.clone();
            let source = input::EmailSource::S3 { bucket: bucket.clone(), key: key.clone() };

            let raw_email = match input::fetch_email(&source).await {
                Ok(data) => data,
                Err(e) => {
                    tracing::error!(error = %e, bucket, key, "Failed to fetch email");
                    return Err(e.to_string().into());
                }
            };

            match process_raw_email(bedrock_client, model, storage, &key, &raw_email).await {
                Ok(manifest) => {
                    tracing::info!(
                        count = manifest.mail_pieces.len(),
                        bucket,
                        key,
                        "Successfully processed email"
                    );
                }
                Err(e) => {
                    tracing::error!(error = %e, bucket, key, "Failed to process email");
                    return Err(e.to_string().into());
                }
            }
        }
    }

    Ok(serde_json::json!({"status": "ok"}))
}
