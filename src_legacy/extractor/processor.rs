use anyhow::Result;
use aws_sdk_bedrockruntime::Client as BedrockClient;
use futures::future::join_all;
use xxhash_rust::xxh3::xxh3_64;

use crate::extractor::analysis::{analyze_image, ModelConfig};
use crate::extractor::storage::Storage;
use crate::email::{group_images_by_piece, is_content_image, parse_email, ExtractedImage};
use crate::models::{Address, ContentHash, EmailManifest, MailImage, MailPiece, MailType, TokenUsage};

/// Core email processing pipeline - used by both CLI and Lambda
/// Input: raw email bytes
/// Output: EmailManifest stored via Storage abstraction
pub async fn process_raw_email(
    bedrock_client: &BedrockClient,
    model: &ModelConfig,
    storage: &Storage,
    source_file: &str,
    raw_email: &[u8],
) -> Result<EmailManifest> {
    let parsed = parse_email(raw_email)?;
    tracing::info!(
        subject = %parsed.info.subject,
        images = parsed.images.len(),
        "Parsed email"
    );

    if let Some(manifest) = storage.load_valid_manifest(&parsed.info).await {
        tracing::info!(subject = %parsed.info.subject, "Skipping - valid manifest exists");
        return Ok(manifest);
    }

    let dir = storage.ensure_email_dir(&parsed.info).await?;
    let groups = group_images_by_piece(parsed.images);

    let all_images: Vec<&ExtractedImage> = groups.values().flatten().collect();
    let email_id = &parsed.info.id();
    let analysis_futures: Vec<_> = all_images.iter()
        .map(|image| analyze_image(bedrock_client, model, &image.data, &image.content_type, email_id))
        .collect();
    let analysis_results = join_all(analysis_futures).await;

    let mut result_iter = analysis_results.into_iter();
    let mut mail_pieces = Vec::new();
    let mut total_usage = TokenUsage::default();

    for (piece_id, images) in &groups {
        let piece_hash = xxh3_64(piece_id.as_bytes());
        let piece_hash_str = format!("{:016x}", piece_hash);
        let piece_dir = storage.ensure_piece_dir(&dir, &piece_hash_str).await?;

        let mut mailer_images: Vec<&ExtractedImage> = Vec::new();
        let mut content_images: Vec<&ExtractedImage> = Vec::new();

        for image in images {
            if is_content_image(&image.filename) {
                content_images.push(image);
            } else {
                mailer_images.push(image);
            }
        }

        if mailer_images.len() > 1 {
            tracing::warn!(piece = %piece_id, count = mailer_images.len(), "Multiple mailer images found for piece, using first");
        }
        if content_images.len() > 1 {
            tracing::warn!(piece = %piece_id, count = content_images.len(), "Multiple content images found for piece, using first");
        }

        let mut mailer: Option<MailImage> = None;
        let mut content: Option<MailImage> = None;
        let mut best_from: Option<Address> = None;
        let mut best_to: Option<Address> = None;
        let mut best_mail_type: MailType = "unknown".to_string();
        let mut best_confidence: f32 = 0.0;
        let mut postmark_date: Option<chrono::NaiveDate> = None;

        for image in images {
            let image_hash = ContentHash {
                value: format!("{:016x}", xxh3_64(&image.data)),
                hash_type: "xxh3".to_string(),
            };

            let is_content = is_content_image(&image.filename);
            let simple_filename = if is_content { "content.jpg" } else { "mailer.jpg" };
            let image_path = storage.store_image(&piece_dir, &piece_hash_str, &image.data, simple_filename).await?;

            let analysis_result = result_iter.next().unwrap();
            let (full_text, error) = match analysis_result {
                Ok((analysis, usage)) => {
                    total_usage.input_tokens += usage.input_tokens;
                    total_usage.output_tokens += usage.output_tokens;
                    if best_to.is_none() {
                        best_to = analysis.to_address;
                    }
                    if best_from.is_none() {
                        best_from = analysis.from_address;
                    }
                    if analysis.confidence.unwrap_or(0.0) > best_confidence {
                        best_confidence = analysis.confidence.unwrap_or(0.0);
                        best_mail_type = analysis.mail_type;
                    }
                    if postmark_date.is_none() {
                        postmark_date = analysis.postmark_date;
                    }
                    (analysis.full_text, None)
                }
                Err(e) => {
                    tracing::warn!(image = %image.filename, error = %e, "Analysis failed");
                    (String::new(), Some(format!("{e}")))
                }
            };

            let mail_image = MailImage {
                hash: image_hash,
                image: image_path,
                full_text,
                error,
            };

            if is_content {
                if content.is_none() {
                    content = Some(mail_image);
                }
            } else {
                if mailer.is_none() {
                    mailer = Some(mail_image);
                }
            }
        }

        mail_pieces.push(MailPiece {
            id: piece_hash_str,
            from_address: best_from,
            to_address: best_to,
            mail_type: best_mail_type,
            confidence: best_confidence,
            postmark_date,
            mailer,
            content,
        });
    }

    let received_date = chrono::NaiveDate::parse_from_str(&parsed.info.date_folder(), "%Y-%m-%d")
        .unwrap_or_else(|_| chrono::Utc::now().date_naive());
    let email_id = format!("{:016x}", xxh3_64(parsed.info.message_id.as_bytes()));
    let manifest = EmailManifest {
        id: email_id,
        model_id: model.model_id.clone(),
        source_file: source_file.to_string(),
        email_subject: parsed.info.subject,
        email_from: parsed.info.from,
        email_date: parsed.info.date,
        received_date,
        email_message_id: parsed.info.message_id,
        processed_at: chrono::Utc::now().to_rfc3339(),
        mail_pieces,
        usage: total_usage,
    };

    storage.store_manifest(&dir, &manifest).await?;
    tracing::info!(count = manifest.mail_pieces.len(), "Processing complete");

    Ok(manifest)
}
