use aws_sdk_sesv2::types::{Body, Content, Destination, EmailContent, Message};

pub const FROM_ADDRESS: &str = "noreply@parmail.thetaone.io";
pub type EmailError = Box<dyn std::error::Error + Send + Sync>;


pub struct SeS {
    client: aws_sdk_sesv2::Client,
}

impl SeS {
    pub async fn new() -> Self {
        let config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
        Self{
            client: aws_sdk_sesv2::Client::new(&config)
        }
    }
    pub async fn send_email(
        &self,
        to: &str,
        subject: &str,
        body_text: &str,
    ) -> Result<(), EmailError> {
        self.client
            .send_email()
            .from_email_address(FROM_ADDRESS)
            .destination(Destination::builder().to_addresses(to).build())
            .content(EmailContent::builder().simple(
                Message::builder()
                    .subject(
                        Content::builder().data(subject).charset("UTF-8").build().expect("subject")
                    )
                    .body(
                        Body::builder().text(Content::builder().data(body_text).charset("UTF-8").build().expect("body")).build()
                    )
                    .build()
            ).build())
            .send()
            .await?;
        Ok(())
    }
}


