use anyhow::Result;
use aws_lambda_events::event::sqs::SqsEvent;
use aws_sdk_s3::Client as S3Client;
use clap::Parser;
use futures::stream::{self, StreamExt};
use lambda_runtime::{service_fn, LambdaEvent};
use parmail::email::{parse_email, Email};
use parmail::input::{fetch_email, EmailSource};
use parmail::models::S3Event;
use parmail::sqs::SqS;

type LambdaError = Box<dyn std::error::Error + Send + Sync>;

/// Known forwarding confirmation senders — these always pass through.
const FORWARDING_PROVIDERS: &[&str] = &[
    "forwarding-noreply@google.com",
    "noreply@microsoft.com",
];

/// USPS sender domains — emails from these are either Informed Delivery or verification.
const USPS_DOMAINS: &[&str] = &[
    "usps.com",
    "usps.gov",
    "informeddelivery.usps.com",
    "email.informeddelivery.usps.com",
];

/// Subjects that indicate a USPS verification email (dead end — delete).
const USPS_VERIFICATION_SUBJECTS: &[&str] = &[
    "verify your email",
    "confirm your email",
    "email verification",
];

/// Subjects that indicate legitimate USPS Informed Delivery content.
const USPS_CONTENT_SUBJECTS: &[&str] = &[
    "daily digest",
    "informed delivery",
];

#[derive(Parser)]
#[command(name = "parmail-gatekeeper", about = "Email gatekeeper Lambda")]
enum Cli {
    /// Run as AWS Lambda (default when deployed)
    Lambda,
    /// Classify a local email file
    Process {
        /// Path to a raw email file
        path: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("parmail_gatekeeper=info".parse().unwrap()),
        )
        .json()
        .init();

    let cli = Cli::parse();
    match cli {
        Cli::Lambda => run_lambda().await,
        Cli::Process { path } => run_local(&path),
    }
}

fn run_local(path: &str) -> Result<()> {
    let raw_email = std::fs::read(path)?;
    let parsed = parse_email(&raw_email)?;

    let status = match classify(&parsed) {
        Classification::PassThrough => "pass",
        Classification::Delete(r) => r,
    };

    println!("{}: {}", status, path);

    Ok(())
}

async fn run_lambda() -> Result<()> {
    let confirmer_queue_url = std::env::var("CONFIRMER_QUEUE_URL")
        .expect("CONFIRMER_QUEUE_URL must be set");
    let sqs = SqS::new(&confirmer_queue_url).await;


    lambda_runtime::run(service_fn(move |event: LambdaEvent<SqsEvent>| {
        let sqs = sqs.clone();
        async move { handle_sqs_event(&sqs, event).await }
    }))
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(())
}

async fn handle_sqs_event(
    sqs: &SqS,
    event: LambdaEvent<SqsEvent>,
) -> std::result::Result<serde_json::Value, LambdaError> {
    let (sqs_event, _context) = event.into_parts();

    let classified = fetch_and_classify(&sqs_event).await;
    let (passed, deleted) = act(sqs, &classified).await;

    Ok(serde_json::json!({"status": "ok", "passed": passed, "deleted": deleted}))
}

struct Classified {
    body: String,
    bucket: String,
    key: String,
    classification: Classification,
}

async fn fetch_and_classify(sqs_event: &SqsEvent) -> Vec<Classified> {
    let sources = sqs_event.records.iter()
        .filter_map(|msg| {
            let body = msg.body.as_deref()?;
            Some((body.to_string(), serde_json::from_str::<S3Event>(body).ok()?))
        })
        .flat_map(|(body, s3_event)| {
            s3_event.records.into_iter().map(move |r| (body.clone(), r))
        })
        .map(|(body, record)| {
            let bucket = record.s3.bucket.name.clone();
            let key = record.s3.object.key.clone();
            let source = EmailSource::S3 {
                bucket: bucket.clone(),
                key: key.clone(),
            };
            (body, bucket, key, source)
        });

    stream::iter(sources)
        .filter_map(|(body, bucket, key, source)| async move {
            let raw = fetch_email(&source).await
                .map_err(|e| tracing::warn!(error = %e, key = key.as_str(), "Failed to fetch email"))
                .ok()?;
            let parsed = parse_email(&raw)
                .map_err(|e| tracing::warn!(error = %e, key = key.as_str(), "Failed to parse email"))
                .ok()?;
            let classification = classify(&parsed);
            tracing::info!(
                from = parsed.info.from_address.as_str(),
                subject = parsed.info.subject.as_str(),
                key = key.as_str(),
                result = match &classification {
                    Classification::PassThrough => "pass",
                    Classification::Delete(r) => r,
                },
                "Classified"
            );
            Some(Classified { body, bucket, key, classification })
        })
        .collect()
        .await
}

async fn act(sqs: &SqS, classified: &[Classified]) -> (u64, u64) {
    let mut passed = 0u64;
    let mut deleted = 0u64;

    for item in classified {
        match &item.classification {
            Classification::PassThrough => {
                if sqs.forward(&item.body).await.is_ok() {
                    passed += 1;
                }
            }
            Classification::Delete(_) => {
                delete_s3_object(&item.bucket, &item.key).await;
                deleted += 1;
            }
        }
    }

    (passed, deleted)
}

enum Classification {
    PassThrough,
    Delete(&'static str),
}

fn classify(email: &Email) -> Classification {
    let from = email.info.from_address.to_lowercase();
    let subject = email.info.subject.to_lowercase();

    // Forwarding confirmations from known providers always pass through
    for provider in FORWARDING_PROVIDERS {
        if from == *provider {
            return Classification::PassThrough;
        }
    }

    // Check if from a USPS domain
    let is_usps_sender = USPS_DOMAINS.iter().any(|d| from.ends_with(d));

    if is_usps_sender {
        // USPS verification emails are dead ends — delete
        for pattern in USPS_VERIFICATION_SUBJECTS {
            if subject.contains(pattern) {
                return Classification::Delete("usps verification email");
            }
        }
        // USPS Informed Delivery — pass through (always forwarded; USPS doesn't deliver to parmail directly)
        return Classification::PassThrough;
    }

    // Not a USPS sender — check if subject looks like forwarded Informed Delivery
    for pattern in USPS_CONTENT_SUBJECTS {
        if subject.contains(pattern) {
            return Classification::PassThrough;
        }
    }

    Classification::Delete("not usps content")
}

async fn delete_s3_object(bucket: &str, key: &str) {
    let config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
    let s3 = S3Client::new(&config);
    if let Err(e) = s3.delete_object().bucket(bucket).key(key).send().await {
        tracing::error!(error = %e, bucket, key, "Failed to delete S3 object");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parmail::email::Header;

    fn make_email(from_address: &str, subject: &str, has_images: bool) -> Email {
        Email {
            info: Header {
                subject: subject.to_string(),
                from: "Test".to_string(),
                from_address: from_address.to_string(),
                resent_from: None,
                date: "2026-06-17T00:00:00Z".to_string(),
                message_id: "test@example.com".to_string(),
            },
            body: None,
            images: if has_images {
                vec![parmail::email::ExtractedImage {
                    filename: "mailer.jpg".to_string(),
                    content_type: "image/jpeg".to_string(),
                    data: vec![0xFF],
                }]
            } else {
                vec![]
            },
        }
    }

    #[test]
    fn test_gmail_forwarding_confirmation_passes() {
        let email = make_email(
            "forwarding-noreply@google.com",
            "Gmail Forwarding Confirmation",
            false,
        );
        assert!(matches!(classify(&email), Classification::PassThrough));
    }

    #[test]
    fn test_o365_forwarding_confirmation_passes() {
        let email = make_email(
            "noreply@microsoft.com",
            "Email forwarding confirmation",
            false,
        );
        assert!(matches!(classify(&email), Classification::PassThrough));
    }

    #[test]
    fn test_usps_verification_deleted() {
        let email = make_email(
            "noreply@usps.com",
            "Verify Your Email Address",
            false,
        );
        assert!(matches!(classify(&email), Classification::Delete("usps verification email")));
    }

    #[test]
    fn test_usps_informed_delivery_passes() {
        let email = make_email(
            "USPSInformeddelivery@email.informeddelivery.usps.com",
            "Your Daily Digest for Mon, Jun 16",
            true,
        );
        assert!(matches!(classify(&email), Classification::PassThrough));
    }

    #[test]
    fn test_forwarded_usps_by_subject_passes() {
        let email = make_email(
            "christian.bongiorno@gmail.com",
            "Your Daily Digest for Mon, Jun 16",
            true,
        );
        assert!(matches!(classify(&email), Classification::PassThrough));
    }

    #[test]
    fn test_forwarded_unknown_subject_deleted() {
        let email = make_email(
            "JohnD@artman.us",
            "FW: something random",
            true,
        );
        assert!(matches!(classify(&email), Classification::Delete("not usps content")));
    }

    #[test]
    fn test_random_spam_deleted() {
        let email = make_email(
            "spammer@evil.com",
            "Buy cheap watches",
            false,
        );
        assert!(matches!(classify(&email), Classification::Delete("not usps content")));
    }

    #[test]
    fn test_unparseable_email_counts_as_delete() {
        // Unparseable emails are handled in the main loop (delete + continue)
        // This test just verifies the classify logic for edge cases
        let email = make_email("", "", false);
        assert!(matches!(classify(&email), Classification::Delete("not usps content")));
    }
}
