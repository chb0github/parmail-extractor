use anyhow::Result;
use clap::Parser;
use futures::stream::{self, StreamExt};
use lambda_runtime::{service_fn, LambdaEvent};
use parmail::ses::SeS;
use parmail::sqs::SqS;
use parmail::confirmer;

use aws_lambda_events::event::sqs::SqsEvent;
use parmail::email::parse_email;
use parmail::input::{fetch_email, EmailSource};
use parmail::models::S3Event;

type LambdaError = Box<dyn std::error::Error + Send + Sync>;

const SUBJECT_PREFIX: &str = "Parmail Forwarding Confirmation - Action Required";
const UNKNOWN_SUBJECT: &str = "Parmail - Unrecognized Forwarding Address";
const UNKNOWN_TEMPLATE: &str = include_str!("templates/unknown_forwarder.txt");

/// USPS sender domains — gatekeeper already validated these, pass through without confirmation.
const USPS_DOMAINS: &[&str] = &[
    "usps.com",
    "usps.gov",
    "informeddelivery.usps.com",
    "email.informeddelivery.usps.com",
];

fn is_usps_sender(parsed: &parmail::email::Email) -> bool {
    USPS_DOMAINS.iter().any(|d| parsed.info.from_address.to_lowercase().ends_with(d))
}


#[derive(Parser)]
#[command(name = "parmail-confirmer", about = "Forwarding confirmation Lambda")]
enum Cli {
    /// Run as AWS Lambda (default when deployed)
    Lambda,
    /// Process a local email file (dry-run mode, prints instead of sending)
    Process {
        /// Path to a raw email file
        path: String,
        /// Directory containing state/confirmed/{email} files (default: results)
        #[arg(long, default_value = "results")]
        state_dir: String,
    },
    /// Mark a forwarder as confirmed locally
    Confirm {
        /// Email address to confirm
        email: String,
        /// Directory containing state/confirmed/{email} files (default: results)
        #[arg(long, default_value = "results")]
        state_dir: String,
    },
    /// List confirmed forwarders
    List {
        /// Directory containing state/confirmed/{email} files (default: results)
        #[arg(long, default_value = "results")]
        state_dir: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("parmail_confirmer=info".parse().unwrap()),
        )
        .json()
        .init();

    let cli = Cli::parse();
    match cli {
        Cli::Lambda => run_lambda().await,
        Cli::Process { path, state_dir } => run_local(&path, &state_dir),
        Cli::Confirm { email, state_dir } => confirm_local(&email, &state_dir),
        Cli::List { state_dir } => list_confirmed(&state_dir),
    }
}

async fn run_lambda() -> Result<()> {
    let extractor_queue_url = std::env::var("EXTRACTOR_QUEUE_URL")
        .expect("EXTRACTOR_QUEUE_URL must be set");
    let sqs = SqS::new(&extractor_queue_url).await;


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
    let ses = SeS::new().await;
    let (sqs_event, _context) = event.into_parts();

    let emails = fetch_emails(&sqs_event).await;
    let sent = send_confirmations(&ses, &emails).await;
    let forwarded = forward_to_extractor(&ses, sqs, &emails).await;

    Ok(serde_json::json!({"status": "ok", "sent": sent, "forwarded": forwarded}))
}

async fn fetch_emails(sqs_event: &SqsEvent) -> Vec<(String, parmail::email::Email)> {
    let sources = sqs_event.records.iter()
        .filter_map(|msg| {
            let body = msg.body.as_deref()?;
            Some((body.to_string(), serde_json::from_str::<S3Event>(body).ok()?))
        })
        .flat_map(|(body, s3_event)| {
            s3_event.records.into_iter().map(move |r| (body.clone(), r))
        })
        .map(|(body, record)| {
            let source = EmailSource::S3 {
                bucket: record.s3.bucket.name.clone(),
                key: record.s3.object.key.clone(),
            };
            (body, source)
        });

    stream::iter(sources)
        .filter_map(|(body, source)| async move {
            let parsed = fetch_email(&source).await
                .map_err(|e| tracing::warn!(error = %e, "Failed to fetch email"))
                .ok()
                .and_then(|raw| parse_email(&raw)
                    .map_err(|e| tracing::warn!(error = %e, "Failed to parse email"))
                    .ok())?;
            Some((body, parsed))
        })
        .collect()
        .await
}

async fn send_confirmations(ses: &SeS, emails: &[(String, parmail::email::Email)]) -> u64 {
    stream::iter(
        emails.iter()
            .filter(|(_, parsed)| confirmer::is_forwarding_request(parsed))
            .filter_map(|(_, parsed)| {
                let (name, fwd_provider) = confirmer::get_forwarding_provider(parsed)?;
                let confirmation = (fwd_provider.extract)(parsed)?;
                tracing::info!(provider = name, originator = confirmation.originator.as_str(), "Detected forwarding confirmation");
                Some((name, fwd_provider.render(name, &confirmation), confirmation.originator))
            })
    )
    .then(|(name, email_body, originator)| {
        let ses = &*ses;
        async move {
            match ses.send_email(&originator, SUBJECT_PREFIX, &email_body).await {
                Ok(_) => { tracing::info!(provider = name, to = originator.as_str(), "Confirmation email sent"); 1u64 }
                Err(e) => { tracing::error!(error = %e, "Failed to send confirmation email"); 0 }
            }
        }
    })
    .fold(0u64, |count, n| async move { count + n })
    .await
}

/// Forward confirmed non-confirmation emails to the extractor queue.
/// Unknown forwarders get a one-time notification, then all their emails are silently dropped.
async fn forward_to_extractor(ses: &SeS, sqs: &SqS, emails: &[(String, parmail::email::Email)]) -> u64 {
    stream::iter(
        emails.iter()
            .filter(|(_, parsed)| !confirmer::is_forwarding_request(parsed))
    )
    .then(|(body, parsed)| {
        let sqs = &*sqs;
        let ses = &*ses;
        async move {
            // USPS senders pass through — gatekeeper already validated them
            if is_usps_sender(parsed) {
                return forward_or_zero(sqs, body).await;
            }
            let forwarder = get_forwarder(parsed);
            if check_s3_exists(&format!("state/confirmed/{}", forwarder)).await {
                return forward_or_zero(sqs, body).await;
            }
            // Not confirmed — notify once, then drop
            notify_unknown_forwarder(ses, forwarder).await;
            0u64
        }
    })
    .fold(0u64, |count, n| async move { count + n })
    .await
}

async fn forward_or_zero(sqs: &SqS, body: &str) -> u64 {
    match sqs.forward(body).await {
        Ok(_) => 1u64,
        Err(e) => { tracing::error!(error = %e, "Failed to forward to extractor queue"); 0 }
    }
}

/// Send a one-time notification to an unknown forwarder, then mark them as notified.
async fn notify_unknown_forwarder(ses: &SeS, forwarder: &str) {
    let notified_key = format!("state/notified/{}", forwarder);
    if check_s3_exists(&notified_key).await {
        tracing::info!(forwarder, "Already notified — silently dropping");
        return;
    }
    let body = UNKNOWN_TEMPLATE.replace("{forwarder}", forwarder);
    match ses.send_email(forwarder, UNKNOWN_SUBJECT, &body).await {
        Ok(_) => tracing::info!(forwarder, "Sent unknown-forwarder notification"),
        Err(e) => { tracing::error!(error = %e, forwarder, "Failed to send notification"); return; }
    }
    mark_s3_state(&notified_key).await;
}

/// Identify the forwarding party from email headers.
/// O365/Bellevue sets Resent-From; Gmail rewrites From to the forwarder's address.
fn get_forwarder(parsed: &parmail::email::Email) -> &str {
    parsed.info.resent_from.as_deref()
        .unwrap_or(&parsed.info.from_address)
}

/// Local dry-run mode: parse a file and print what the confirmer thinks the state is.
/// Only checks local filesystem for confirmed state — no S3 calls.
fn run_local(path: &str, state_dir: &str) -> Result<()> {
    let raw_email = std::fs::read(path)?;
    let parsed = parse_email(&raw_email)?;

    let status = if confirmer::is_forwarding_request(&parsed) {
        "confirmation-request"
    } else if is_usps_sender(&parsed) {
        "pass"
    } else {
        let forwarder = get_forwarder(&parsed);
        if is_confirmed_local(state_dir, forwarder) { "confirmed" } else { "unconfirmed" }
    };

    println!("{}: {}", status, path);
    Ok(())
}

fn is_confirmed_local(state_dir: &str, forwarder: &str) -> bool {
    std::path::Path::new(&format!("{}/state/confirmed/{}", state_dir, forwarder)).exists()
}

fn confirm_local(email: &str, state_dir: &str) -> Result<()> {
    let dir = format!("{}/state/confirmed", state_dir);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(format!("{}/{}", dir, email), "")?;
    println!("confirmed: {}", email);
    Ok(())
}

fn list_confirmed(state_dir: &str) -> Result<()> {
    let dir = format!("{}/state/confirmed", state_dir);
    let path = std::path::Path::new(&dir);
    if !path.exists() {
        println!("No confirmed forwarders.");
        return Ok(());
    }
    std::fs::read_dir(path)?
        .filter_map(|e| e.ok())
        .for_each(|e| println!("{}", e.file_name().to_string_lossy()));
    Ok(())
}

fn bucket_name() -> String {
    std::env::var("BUCKET_NAME").expect("BUCKET_NAME must be set")
}

async fn check_s3_exists(key: &str) -> bool {
    let config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
    let s3 = aws_sdk_s3::Client::new(&config);
    s3.head_object()
        .bucket(bucket_name())
        .key(key)
        .send()
        .await
        .is_ok()
}

async fn mark_s3_state(key: &str) {
    let config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
    let s3 = aws_sdk_s3::Client::new(&config);
    if let Err(e) = s3.put_object()
        .bucket(bucket_name())
        .key(key)
        .body(aws_sdk_s3::primitives::ByteStream::from_static(b""))
        .send()
        .await
    {
        tracing::error!(error = %e, key, "Failed to write state key");
    }
}
