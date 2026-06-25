use anyhow::Result;
use aws_sdk_bedrockruntime::Client as BedrockClient;
use aws_sdk_s3::Client as S3Client;
use clap::{Parser, Subcommand};
use futures::stream::{self, StreamExt};
use std::sync::atomic::{AtomicU64, Ordering};

use parmail::extractor::analysis::ModelConfig;
use parmail::extractor::extractor;
use parmail::extractor::output::{Output, Verbosity};
use parmail::extractor::processor;
use parmail::extractor::storage::Storage;
use parmail::extractor::validate;
use parmail::input::{self, fetch_email, resolve_sources};

#[derive(Parser)]
#[command(name = "parmail", about = "USPS Informed Delivery mail image processor")]
struct Cli {
    /// Increase verbosity (-v, -vv)
    #[arg(short, long, action = clap::ArgAction::Count, global = true, conflicts_with = "quiet")]
    verbose: u8,
    /// Decrease verbosity (-q, -qq)
    #[arg(short, long, action = clap::ArgAction::Count, global = true, conflicts_with = "verbose")]
    quiet: u8,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run as an AWS Lambda function (S3 event trigger)
    Lambda,
    /// Validate AWS credentials and Bedrock access
    Validate,
    /// Process .eml files from local paths, directories, or s3:// URIs
    Process {
        /// Directory or s3://bucket/prefix to store images and metadata
        #[arg(short, long)]
        storage_dir: Option<String>,
        /// Concurrent email processing limit
        #[arg(short, long, default_value = "2")]
        concurrency: usize,
        /// Bedrock model ID to use for analysis
        #[arg(short, long)]
        model: Option<String>,
        /// Path to models config file
        #[arg(long, default_value = "models.default.json")]
        models_file: String,
        /// Save raw model responses to disk for debugging
        #[arg(long, default_value = "false")]
        save_responses: bool,
        /// Paths to .eml files, directories, or s3://bucket/prefix URIs
        #[arg(required = true, trailing_var_arg = true)]
        paths: Vec<String>,
    },
}

async fn process_emails(
    paths: Vec<String>,
    storage_dir: Option<String>,
    concurrency: usize,
    model: Option<String>,
    models_file: String,
    save_responses: bool,
    verbosity: Verbosity,
) -> Result<()> {
    let storage_dir = match (&storage_dir, &model) {
        (Some(dir), _) => dir.clone(),
        (None, Some(id)) => {
            let short = id.rsplit('.').next().unwrap_or(id).split(':').next().unwrap_or(id);
            format!("results/{short}")
        }
        (None, None) => "./data".to_string(),
    };

    let model_config = match model {
        Some(id) => ModelConfig::load(&models_file, &id, save_responses, &storage_dir)?,
        None => ModelConfig::default_config(&storage_dir),
    };

    let config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
    let bedrock_client = BedrockClient::new(&config);

    let needs_s3 = paths.iter().any(|p| p.starts_with("s3://"))
        || storage_dir.starts_with("s3://");
    let s3_client = match needs_s3 {
        true => Some(S3Client::new(&config)),
        false => None,
    };

    let storage = Storage::from_uri(&storage_dir, s3_client.clone())?;
    let sources = resolve_sources(&paths).await?;
    let out = Output::new(verbosity, std::io::IsTerminal::is_terminal(&std::io::stderr()), sources.len() as u64);
    let errors = AtomicU64::new(0);
    let total = sources.len() as u64;

    process_sources(&bedrock_client, &model_config, &storage, &sources, &out, &errors, concurrency).await;

    out.finish(total, errors.load(Ordering::Relaxed));
    Ok(())
}

async fn process_sources(
    bedrock_client: &BedrockClient,
    model_config: &ModelConfig,
    storage: &Storage,
    sources: &[input::EmailSource],
    out: &Output,
    errors: &AtomicU64,
    concurrency: usize,
) {
    stream::iter(sources.iter())
        .for_each_concurrent(concurrency, |source| async move {
            let name = source.short_name();
            let raw = match fetch_email(source).await {
                Ok(data) => data,
                Err(e) => {
                    out.error(&format!("{source}: {e}"));
                    errors.fetch_add(1, Ordering::Relaxed);
                    return;
                }
            };

            match processor::process_raw_email(bedrock_client, model_config, storage, &name, &raw).await {
                Ok(manifest) => {
                    out.file_done(&manifest.received_date.to_string(), &manifest.email_message_id, manifest.mail_pieces.len(), true);
                }
                Err(e) => {
                    out.file_done(&name, "", 0, false);
                    out.error(&format!("{source}: {e}"));
                    errors.fetch_add(1, Ordering::Relaxed);
                }
            }
        })
        .await;
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_target(false)
        .without_time()
        .init();

    let cli = Cli::parse();

    let verbosity = Verbosity::from_flags(cli.verbose, cli.quiet);

    match cli.command {
        Commands::Lambda => {
            extractor::run_lambda().await?;
        }
        Commands::Validate => {
            let config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
            let bedrock_client = aws_sdk_bedrockruntime::Client::new(&config);
            validate::validate_aws(&bedrock_client).await?;
            eprintln!("All AWS resources validated successfully.");
        }
        Commands::Process { paths, storage_dir, concurrency, model, models_file, save_responses } => {
            process_emails(paths, storage_dir, concurrency, model, models_file, save_responses, verbosity).await?;
        }
    }

    Ok(())
}
