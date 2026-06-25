use anyhow::{Context, Result, bail};
use aws_sdk_bedrockruntime::types::{
    ContentBlock, ConversationRole, ImageBlock, ImageFormat, ImageSource, Message,
    Tool, ToolChoice, ToolConfiguration, ToolInputSchema, ToolSpecification,
    SpecificToolChoice,
};
use aws_sdk_bedrockruntime::Client as BedrockClient;
use aws_smithy_types::Document;
use backon::{ExponentialBuilder, Retryable};
use std::collections::HashMap;
use std::path::Path;

use crate::models::{Address, MailType, TokenUsage};

const DEFAULT_MODEL_ID: &str = "us.anthropic.claude-haiku-4-5-20251001-v1:0";

const ANALYSIS_PROMPT: &str = "Analyze this scanned image of a piece of physical mail. \
Extract the sender address, recipient address, classify the mail type, transcribe all visible text, \
and extract the postmark date if visible. USPS often redacts the recipient address with a white rectangle — \
if you see that, set to_address fields to null.";

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ModelEntry {
    pub format: String,
    pub prompt: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ModelConfig {
    pub model_id: String,
    pub entry: ModelEntry,
    pub save_responses: bool,
    pub storage_dir: String,
}

impl ModelConfig {
    pub fn load(models_file: &str, model_id: &str, save_responses: bool, storage_dir: &str) -> Result<Self> {
        assert!(!models_file.is_empty(), "models_file must not be empty");
        assert!(!model_id.is_empty(), "model_id must not be empty");

        let path = Path::new(models_file);
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read models file: {models_file}"))?;
        let models: HashMap<String, ModelEntry> = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse models file: {models_file}"))?;
        let entry = models.get(model_id)
            .with_context(|| format!("Model '{model_id}' not found in {models_file}"))?
            .clone();

        Ok(Self { model_id: model_id.to_string(), entry, save_responses, storage_dir: storage_dir.to_string() })
    }

    pub fn default_config(storage_dir: &str) -> Self {
        Self {
            model_id: DEFAULT_MODEL_ID.to_string(),
            entry: ModelEntry { format: "tool_use".to_string(), prompt: None },
            save_responses: false,
            storage_dir: storage_dir.to_string(),
        }
    }
}

fn tool_schema() -> Document {
    Document::Object(
        [
            ("type".into(), Document::String("object".into())),
            ("properties".into(), Document::Object(
                [
                    ("from_address".into(), address_schema("The sender's return address")),
                    ("to_address".into(), address_schema("The recipient's delivery address")),
                    ("mail_type".into(), Document::Object(
                        [
                            ("type".into(), Document::String("string".into())),
                            ("enum".into(), Document::Array(vec![
                                Document::String("advertising".into()),
                                Document::String("political".into()),
                                Document::String("personal".into()),
                                Document::String("financial".into()),
                                Document::String("government".into()),
                                Document::String("unknown".into()),
                            ])),
                            ("description".into(), Document::String("Mail classification".into())),
                        ].into_iter().collect()
                    )),
                    ("full_text".into(), Document::Object(
                        [
                            ("type".into(), Document::String("string".into())),
                            ("description".into(), Document::String("ALL text visible on the image, transcribed exactly".into())),
                        ].into_iter().collect()
                    )),
                    ("confidence".into(), Document::Object(
                        [
                            ("type".into(), Document::String("number".into())),
                            ("description".into(), Document::String("Confidence in classification, 0.0 to 1.0".into())),
                        ].into_iter().collect()
                    )),
                    ("postmark_date".into(), Document::Object(
                        [
                            ("type".into(), Document::String("string".into())),
                            ("description".into(), Document::String("Postmark date in YYYY-MM-DD format, or null if not visible".into())),
                        ].into_iter().collect()
                    )),
                ].into_iter().collect()
            )),
            ("required".into(), Document::Array(vec![
                Document::String("from_address".into()),
                Document::String("to_address".into()),
                Document::String("mail_type".into()),
                Document::String("full_text".into()),
                Document::String("confidence".into()),
            ])),
        ].into_iter().collect()
    )
}

fn schema_as_json_string() -> String {
    let schema = tool_schema();
    let value = document_to_value(&schema);
    serde_json::to_string_pretty(&value).unwrap()
}

fn address_schema(description: &str) -> Document {
    Document::Object(
        [
            ("type".into(), Document::String("object".into())),
            ("description".into(), Document::String(description.into())),
            ("properties".into(), Document::Object(
                [
                    ("name".into(), Document::Object([("type".into(), Document::String("string".into()))].into_iter().collect())),
                    ("street".into(), Document::Object([("type".into(), Document::String("string".into()))].into_iter().collect())),
                    ("city".into(), Document::Object([("type".into(), Document::String("string".into()))].into_iter().collect())),
                    ("state".into(), Document::Object([("type".into(), Document::String("string".into()))].into_iter().collect())),
                    ("zip".into(), Document::Object([("type".into(), Document::String("string".into()))].into_iter().collect())),
                    ("resolved".into(), Document::Object([
                        ("type".into(), Document::String("boolean".into())),
                        ("description".into(), Document::String("True if address was successfully extracted and parsed".into()))
                    ].into_iter().collect())),
                ].into_iter().collect()
            )),
            ("required".into(), Document::Array(vec![
                Document::String("resolved".into()),
            ])),
        ].into_iter().collect()
    )
}

#[derive(serde::Deserialize)]
pub struct AnalysisResponse {
    pub from_address: Option<Address>,
    pub to_address: Option<Address>,
    #[serde(default = "default_mail_type")]
    pub mail_type: MailType,
    #[serde(default, deserialize_with = "deserialize_null_as_empty")]
    pub full_text: String,
    pub confidence: Option<f32>,
    #[serde(default, deserialize_with = "deserialize_lenient_date")]
    pub postmark_date: Option<chrono::NaiveDate>,
}

fn deserialize_null_as_empty<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

fn deserialize_lenient_date<'de, D>(deserializer: D) -> Result<Option<chrono::NaiveDate>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    let opt = Option::<String>::deserialize(deserializer)?;
    match opt {
        None => Ok(None),
        Some(s) => {
            let formats = ["%Y-%m-%d", "%m/%d/%Y", "%m-%d-%y"];
            for fmt in &formats {
                if let Ok(date) = chrono::NaiveDate::parse_from_str(&s, fmt) {
                    return Ok(Some(date));
                }
            }
            Ok(None)
        }
    }
}

pub async fn analyze_image(
    client: &BedrockClient,
    model: &ModelConfig,
    image_data: &[u8],
    content_type: &str,
    email_id: &str,
) -> Result<(AnalysisResponse, TokenUsage)> {
    assert!(!image_data.is_empty(), "image_data must not be empty");
    assert!(!content_type.is_empty(), "content_type must not be empty");
    assert!(!email_id.is_empty(), "email_id must not be empty");

    let format = match content_type {
        "image/jpeg" | "image/jpg" => ImageFormat::Jpeg,
        "image/png" => ImageFormat::Png,
        "image/gif" => ImageFormat::Gif,
        "image/webp" => ImageFormat::Webp,
        _ => ImageFormat::Jpeg,
    };

    let image_block = ContentBlock::Image(
        ImageBlock::builder()
            .format(format)
            .source(ImageSource::Bytes(
                aws_sdk_bedrockruntime::primitives::Blob::new(image_data.to_vec()),
            ))
            .build()
            .context("Failed to build image block")?,
    );

    let prompt_text = match entry_format(&model.entry) {
        "tool_use" => ANALYSIS_PROMPT.to_string(),
        "json_prompt" => {
            let base = model.entry.prompt.as_deref().unwrap_or(ANALYSIS_PROMPT);
            format!("{base}\n\nRespond with ONLY a JSON object matching this schema (no markdown fences, no explanation):\n{}", schema_as_json_string())
        }
        other => bail!("Unknown model format: {other}"),
    };

    let text_block = ContentBlock::Text(prompt_text);

    let message = Message::builder()
        .role(ConversationRole::User)
        .content(image_block)
        .content(text_block)
        .build()
        .context("Failed to build message")?;

    let use_tool = entry_format(&model.entry) == "tool_use";

    let response = (|| async {
        let mut req = client.converse()
            .model_id(&model.model_id)
            .messages(message.clone())
            .set_guardrail_config(None);
        match use_tool {
            true => {
                let tool_config = ToolConfiguration::builder()
                    .tools(Tool::ToolSpec(
                        ToolSpecification::builder()
                            .name("analyze_mail")
                            .description("Extract structured information from a scanned mail image")
                            .input_schema(ToolInputSchema::Json(tool_schema()))
                            .build()
                            .expect("tool spec"),
                    ))
                    .tool_choice(ToolChoice::Tool(
                        SpecificToolChoice::builder()
                            .name("analyze_mail")
                            .build()
                            .expect("tool choice"),
                    ))
                    .build()
                    .expect("tool config");
                req = req.tool_config(tool_config);
            }
            false => {}
        }
        req.send().await.map_err(|e| {
            tracing::warn!(model_id = %model.model_id, error = ?e, "Bedrock call failed, will retry");
            e
        })
    })
    .retry(ExponentialBuilder::default().with_max_times(3))
    .await
    .context("Bedrock converse API call failed after retries")?;

    let usage = response.usage().map(|u| TokenUsage {
        input_tokens: u.input_tokens() as u64,
        output_tokens: u.output_tokens() as u64,
    }).unwrap_or_default();

    let output = response
        .output()
        .context("No output in response")?;

    let msg_content = match output {
        aws_sdk_bedrockruntime::types::ConverseOutput::Message(msg) => msg.content().to_vec(),
        _ => Vec::new(),
    };

    let raw_json = match msg_content.iter().find_map(|block| match block {
        ContentBlock::ToolUse(tu) => Some(document_to_value(tu.input())),
        ContentBlock::Text(t) => extract_json(t),
        _ => None,
    }) {
        Some(json) => json,
        None => {
            let raw_text: String = msg_content.iter().filter_map(|block| match block {
                ContentBlock::Text(t) => Some(t.as_str()),
                _ => None,
            }).collect::<Vec<_>>().join("\n");

            // Try fixing common escape issues
            let fixed_text = raw_text.replace(r"\'", "'");
            if let Some(json) = extract_json(&fixed_text) {
                json
            } else {
                if !raw_text.is_empty() {
                    save_unparseable_response(&model.storage_dir, &model.model_id, email_id, &raw_text);
                }

                let snippet: String = raw_text.chars().take(200).collect();
                anyhow::bail!("No parseable response from model: {snippet}");
            }
        }
    };

    if model.save_responses {
        save_raw_response(&model.storage_dir, &model.model_id, &raw_json);
    }

    let normalized = unwrap_schema_echo(&raw_json);

    let parsed: AnalysisResponse = serde_json::from_value(normalized.clone()).map_err(|e| {
        tracing::error!(
            model_id = %model.model_id,
            raw_response = %raw_json,
            error = %e,
            "Failed to parse response"
        );
        anyhow::anyhow!("Failed to parse response: {e}")
    })?;

    Ok((parsed, usage))
}

fn save_raw_response(storage_dir: &str, model_id: &str, json: &serde_json::Value) {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let dir = Path::new(storage_dir).join("_responses");
    let _ = std::fs::create_dir_all(&dir);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let filename = format!("{model_id}_{seq:06}.json");
    let path = dir.join(&filename);
    match std::fs::File::create(&path) {
        Ok(mut f) => { let _ = f.write_all(serde_json::to_string_pretty(json).unwrap_or_default().as_bytes()); }
        Err(e) => tracing::warn!(path = %path.display(), error = %e, "Failed to save response"),
    }
}

fn save_unparseable_response(storage_dir: &str, model_id: &str, email_id: &str, text: &str) {
    use std::io::Write;

    let base = Path::new(storage_dir).parent().unwrap_or(Path::new("."));
    let dir = base.join("_unparseable").join(model_id);
    let _ = std::fs::create_dir_all(&dir);
    let filename = format!("{email_id}.json");
    let path = dir.join(&filename);
    match std::fs::File::create(&path) {
        Ok(mut f) => { let _ = f.write_all(text.as_bytes()); }
        Err(e) => tracing::warn!(path = %path.display(), error = %e, "Failed to save unparseable response"),
    }
}

fn default_mail_type() -> MailType {
    "unknown".to_string()
}

fn entry_format(entry: &ModelEntry) -> &str {
    &entry.format
}

fn extract_json(text: &str) -> Option<serde_json::Value> {
    let trimmed = text.trim();
    let json_str = match trimmed.starts_with("```") {
        true => {
            let start = trimmed.find('\n').map(|i| i + 1).unwrap_or(0);
            let end = trimmed.rfind("```").unwrap_or(trimmed.len());
            &trimmed[start..end]
        }
        false => trimmed,
    };
    serde_json::from_str(json_str.trim()).ok()
}

fn unwrap_schema_echo(json: &serde_json::Value) -> serde_json::Value {
    let result = unwrap_schema_inner(json);
    normalize_null_strings(&result)
}

fn unwrap_schema_inner(json: &serde_json::Value) -> serde_json::Value {
    let obj = match json.as_object() {
        Some(o) => o,
        None => return json.clone(),
    };

    // Schema keys that should be stripped if the model echoed its schema alongside real data
    let schema_keys: &[&str] = &["properties", "required", "type", "description"];

    // If the object has a "properties" key, check if it also has real data fields alongside it
    if obj.contains_key("properties") {
        let has_data_fields = obj.keys().any(|k| !schema_keys.contains(&k.as_str()));
        if has_data_fields {
            // Model mixed schema echo with actual data — strip schema keys, keep data
            let stripped: serde_json::Map<String, serde_json::Value> = obj.iter()
                .filter(|(k, _)| !schema_keys.contains(&k.as_str()))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            return serde_json::Value::Object(stripped);
        }

        // Pure schema echo — extract values from properties definitions
        if let Some(props) = obj.get("properties").and_then(|p| p.as_object()) {
            let extracted: serde_json::Map<String, serde_json::Value> = props.iter()
                .map(|(k, v)| (k.clone(), extract_property_value(v)))
                .collect();
            if let Some(top_val) = obj.get("value").and_then(|v| v.as_object()) {
                let mut merged = extracted;
                for (k, v) in top_val {
                    merged.insert(k.clone(), v.clone());
                }
                return serde_json::Value::Object(merged);
            }
            return serde_json::Value::Object(extracted);
        }
    }

    // Top-level "value" key holds the actual response
    if let Some(val) = obj.get("value") {
        return val.clone();
    }

    json.clone()
}

fn extract_property_value(v: &serde_json::Value) -> serde_json::Value {
    let inner = match v.as_object() {
        Some(o) => o,
        None => return v.clone(),
    };

    // Has explicit "value" key — use it
    if let Some(actual) = inner.get("value") {
        return actual.clone();
    }

    // Has "properties" with sub-fields — recurse (nested address object)
    if let Some(sub_props) = inner.get("properties").and_then(|p| p.as_object()) {
        // Check if sub-properties have actual values or are just type definitions
        let has_values = sub_props.values().any(|sv| {
            match sv.as_object() {
                Some(so) => !so.contains_key("type") || so.contains_key("value"),
                None => true, // scalar = actual value
            }
        });
        match has_values {
            true => {
                let fields: serde_json::Map<String, serde_json::Value> = sub_props.iter()
                    .map(|(k, sv)| (k.clone(), extract_property_value(sv)))
                    .collect();
                return serde_json::Value::Object(fields);
            }
            false => return serde_json::Value::Null, // all fields are schema defs with no values
        }
    }

    // Looks like a bare schema definition (has "type"/"description" but no data)
    if inner.contains_key("type") && !inner.contains_key("city") && !inner.contains_key("name") {
        return serde_json::Value::Null;
    }

    // Otherwise it's a real object (e.g. address with city/state/etc directly)
    v.clone()
}

fn normalize_null_strings(json: &serde_json::Value) -> serde_json::Value {
    match json {
        serde_json::Value::String(s) if s == "null" || s == "None" || s == "N/A" => serde_json::Value::Null,
        serde_json::Value::Object(map) => {
            let normalized: serde_json::Map<String, serde_json::Value> = map.iter()
                .map(|(k, v)| (k.clone(), normalize_null_strings(v)))
                .collect();
            serde_json::Value::Object(normalized)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(normalize_null_strings).collect())
        }
        _ => json.clone(),
    }
}

fn document_to_value(doc: &Document) -> serde_json::Value {
    match doc {
        Document::Object(map) => {
            let obj: serde_json::Map<String, serde_json::Value> = map
                .iter()
                .map(|(k, v)| (k.clone(), document_to_value(v)))
                .collect();
            serde_json::Value::Object(obj)
        }
        Document::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(document_to_value).collect())
        }
        Document::Number(n) => {
            serde_json::Number::from_f64(n.to_f64_lossy())
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null)
        }
        Document::String(s) => serde_json::Value::String(s.clone()),
        Document::Bool(b) => serde_json::Value::Bool(*b),
        Document::Null => serde_json::Value::Null,
    }
}
