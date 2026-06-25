// Re-export from library for backward compatibility with local tests
pub use parmail::confirmer::*;

#[cfg(test)]
mod tests {
    use super::*;
    use parmail::email::parse_email;

    #[test]
    fn test_detect_gmail_forwarding_recent() {
        let raw = include_bytes!("../../../tests/fixtures/confirm_1.eml");
        let parsed = parse_email(raw).unwrap();
        assert!(is_forwarding_request(&parsed));
        let (name, provider) = get_forwarding_provider(&parsed).unwrap();
        assert_eq!(name, "Gmail");
        let confirmation = (provider.extract)(&parsed).unwrap();
        assert!(confirmation.originator.contains("REPLACED"));
        assert!(confirmation.confirm_url.contains("/mail/vf-"), "unexpected URL: {}", confirmation.confirm_url);
    }

    #[test]
    fn test_detect_gmail_forwarding_original() {
        let raw = include_bytes!("../../../tests/fixtures/confirm_2.eml");
        let parsed = parse_email(raw).unwrap();
        assert!(is_forwarding_request(&parsed));
        let (name, provider) = get_forwarding_provider(&parsed).unwrap();
        assert_eq!(name, "Gmail");
        let confirmation = (provider.extract)(&parsed).unwrap();
        assert!(confirmation.originator.contains("REPLACED"));
        assert!(confirmation.confirm_url.contains("/mail/vf-"), "unexpected URL: {}", confirmation.confirm_url);
    }

    #[test]
    fn test_regular_email_not_detected() {
        let email = parmail::email::Email {
            info: parmail::email::Header {
                subject: "Your Daily Digest for Mon, Jun 16".to_string(),
                from: "USPS Informed Delivery".to_string(),
                from_address: "testuser-REPLACED@gmail.com".to_string(),
                resent_from: None,
                date: "2026-06-16T19:00:00Z".to_string(),
                message_id: "test@example.com".to_string(),
            },
            body: Some("Here is your daily mail scan.".to_string()),
            images: vec![],
        };
        assert!(!is_forwarding_request(&email));
        assert!(get_forwarding_provider(&email).is_none());
    }
}
