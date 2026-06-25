use anyhow::{Context, Result};
use indexmap::IndexMap;
use mail_parser::{Address as MailAddress, MessageParser, MimeHeaders, PartType};
use xxhash_rust::xxh3::xxh3_64;

pub struct ExtractedImage {
    pub filename: String,
    pub content_type: String,
    pub data: Vec<u8>,
}

pub struct Header {
    pub subject: String,
    pub from: String,
    pub from_address: String,
    pub resent_from: Option<String>,
    pub date: String,
    pub message_id: String,
}

impl Header {
    pub fn id(&self) -> String {
        format!("{:016x}", xxh3_64(self.message_id.as_bytes()))
    }

    pub fn date_folder(&self) -> String {
        self.date
            .split('T')
            .next()
            .unwrap_or(&self.date)
            .to_string()
    }
}

pub struct Email {
    pub info: Header,
    pub body: Option<String>,
    pub images: Vec<ExtractedImage>,
}

pub fn parse_email(raw_email: &[u8]) -> Result<Email> {
    let message = MessageParser::default()
        .parse(raw_email)
        .context("Failed to parse email")?;

    let subject = message.subject().unwrap_or("unknown").to_string();

    let (from, from_address) = extract_from(&message);
    let resent_from = extract_resent_from(&message);

    let date = extract_date(&message);

    let message_id = message.message_id().unwrap_or("unknown").to_string();

    let body = extract_body(&message);
    let images = extract_images(&message);

    Ok(Email {
        info: Header {
            subject,
            from,
            from_address,
            resent_from,
            date,
            message_id,
        },
        body,
        images,
    })
}

fn extract_resent_from(message: &mail_parser::Message) -> Option<String> {
    match message.resent_from() {
        Some(MailAddress::List(addrs)) => {
            addrs.first()
                .and_then(|a| a.address.as_ref())
                .map(|a| a.to_string())
        }
        _ => None,
    }
}

fn extract_date(message: &mail_parser::Message) -> String {
    message
        .date()
        .map(|d| {
            format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
                d.year, d.month, d.day, d.hour, d.minute, d.second
            )
        })
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339())
}

/// Returns (display_name_or_address, raw_email_address)
fn extract_from(message: &mail_parser::Message) -> (String, String) {
    match message.from() {
        Some(MailAddress::List(addrs)) => {
            let first = addrs.first();
            let display = first
                .and_then(|a| {
                    a.name
                        .as_ref()
                        .map(|n| n.to_string())
                        .or_else(|| a.address.as_ref().map(|a| a.to_string()))
                })
                .unwrap_or_else(|| "unknown".to_string());
            let address = first
                .and_then(|a| a.address.as_ref().map(|a| a.to_string()))
                .unwrap_or_else(|| "unknown".to_string());
            (display, address)
        }
        Some(MailAddress::Group(groups)) => {
            let name = groups
                .first()
                .and_then(|g| g.name.as_ref())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "unknown".to_string());
            (name.clone(), name)
        }
        _ => ("unknown".to_string(), "unknown".to_string()),
    }
}

fn extract_body(message: &mail_parser::Message) -> Option<String> {
    let body: String = (0..message.text_body.len())
        .filter_map(|i| message.body_text(i))
        .collect::<Vec<_>>()
        .join("");
    if body.is_empty() { None } else { Some(body) }
}

fn extract_images(message: &mail_parser::Message) -> Vec<ExtractedImage> {
    message
        .parts
        .iter()
        .filter(|part| extract_content_type(part).starts_with("image/"))
        .filter_map(|part| {
            let data = match &part.body {
                PartType::Binary(bytes) | PartType::InlineBinary(bytes) => bytes.to_vec(),
                _ => return None,
            };
            if data.is_empty() {
                return None;
            }
            Some(ExtractedImage {
                filename: extract_filename(part),
                content_type: extract_content_type(part),
                data,
            })
        })
        .collect()
}

fn extract_content_type(part: &mail_parser::MessagePart) -> String {
    part.content_type()
        .map(|ct| {
            let main = ct.ctype();
            let sub = ct.subtype().unwrap_or("octet-stream");
            format!("{main}/{sub}")
        })
        .unwrap_or_else(|| "application/octet-stream".to_string())
}

fn extract_filename(part: &mail_parser::MessagePart) -> String {
    part.attachment_name()
        .or_else(|| {
            part.content_id()
                .map(|id| id.trim_matches(|c| c == '<' || c == '>'))
        })
        .unwrap_or("unnamed.jpg")
        .to_string()
}

pub fn is_content_image(filename: &str) -> bool {
    filename.starts_with("ra_0_") || filename.starts_with("content-")
}

pub fn extract_piece_id(filename: &str) -> String {
    let stem = filename
        .strip_suffix(".jpg")
        .or_else(|| filename.strip_suffix(".jpeg"))
        .or_else(|| filename.strip_suffix(".png"))
        .unwrap_or(filename);

    if let Some(id) = stem.strip_prefix("mailer-") {
        return id.to_string();
    }
    if let Some(id) = stem.strip_prefix("content-") {
        return id.to_string();
    }
    if let Some(id) = stem.strip_prefix("ra_0_") {
        return id.to_string();
    }

    stem.to_string()
}

pub fn group_images_by_piece(images: Vec<ExtractedImage>) -> IndexMap<String, Vec<ExtractedImage>> {
    let mut groups: IndexMap<String, Vec<ExtractedImage>> = IndexMap::new();
    for image in images {
        let piece_id = extract_piece_id(&image.filename);
        groups.entry(piece_id).or_default().push(image);
    }
    groups
}

/// Extract mailer and content images from raw email bytes
/// Returns borrowed slices (mailer_bytes, content_bytes) for the first mail piece
/// If email contains multiple pieces, only the first piece's images are returned
/// Either or both images can be None if not present in the email
pub fn get_images(parsed: &Email) -> (Option<&[u8]>, Option<&[u8]>) {
    if parsed.images.is_empty() {
        return (None, None);
    }

    let groups = group_images_by_piece_ref(&parsed.images);
    let first_piece = match groups.into_iter().next() {
        Some((_, images)) => images,
        None => return (None, None),
    };

    let mut mailer: Option<&[u8]> = None;
    let mut content: Option<&[u8]> = None;

    for image in first_piece {
        match (is_content_image(&image.filename), content, mailer) {
            (true, None, _) => content = Some(&image.data),
            (false, _, None) => mailer = Some(&image.data),
            _ => {} // Skip if we already have this image type
        }
    }

    (mailer, content)
}

fn group_images_by_piece_ref(
    images: &[ExtractedImage],
) -> indexmap::IndexMap<String, Vec<&ExtractedImage>> {
    let mut groups: indexmap::IndexMap<String, Vec<&ExtractedImage>> = indexmap::IndexMap::new();
    for image in images {
        let piece_id = extract_piece_id(&image.filename);
        groups.entry(piece_id).or_default().push(image);
    }
    groups
}
