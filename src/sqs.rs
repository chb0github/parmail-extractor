pub type SqsError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Clone)]
pub struct SqS {
    client: aws_sdk_sqs::Client,
    queue_url: String,
}

impl SqS {
    pub async fn new(queue_url: &str) -> Self {
        let config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
        Self {
            client: aws_sdk_sqs::Client::new(&config),
            queue_url: queue_url.to_string(),
        }
    }

    pub async fn forward(&self, body: &str) -> Result<(), SqsError> {
        self.client
            .send_message()
            .queue_url(&self.queue_url)
            .message_body(body)
            .send()
            .await?;
        Ok(())
    }
}
