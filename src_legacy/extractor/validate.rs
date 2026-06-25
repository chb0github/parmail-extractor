use anyhow::{Context, Result};
use aws_sdk_bedrockruntime::Client as BedrockClient;

const MODEL_ID: &str = "anthropic.claude-sonnet-4-20250514";

pub async fn validate_aws(bedrock_client: &BedrockClient) -> Result<()> {
    tracing::info!("Validating AWS configuration...");

    validate_credentials().await?;
    validate_bedrock(bedrock_client).await?;

    tracing::info!("AWS validation passed");
    Ok(())
}

async fn validate_credentials() -> Result<()> {
    let config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
    let sts = aws_sdk_sts::Client::new(&config);

    let identity = sts
        .get_caller_identity()
        .send()
        .await
        .context("AWS credentials not configured or expired")?;

    tracing::info!(
        account = identity.account().unwrap_or("unknown"),
        arn = identity.arn().unwrap_or("unknown"),
        "AWS credentials valid"
    );

    Ok(())
}

async fn validate_bedrock(client: &BedrockClient) -> Result<()> {
    use aws_sdk_bedrockruntime::types::{ContentBlock, ConversationRole, Message};

    let message = Message::builder()
        .role(ConversationRole::User)
        .content(ContentBlock::Text("Say OK".to_string()))
        .build()
        .context("Failed to build test message")?;

    client
        .converse()
        .model_id(MODEL_ID)
        .messages(message)
        .send()
        .await
        .context(format!(
            "Bedrock model {} is not accessible. Check that the model is enabled in your region and your IAM role has bedrock:InvokeModel permission.",
            MODEL_ID
        ))?;

    tracing::info!(model = MODEL_ID, "Bedrock model accessible");
    Ok(())
}
