use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use nca_common::config::{
    CustomProviderConfig, CustomProviderConfigError, NcaConfig, ProviderCompatibility,
    custom_provider_endpoint, normalize_custom_provider_base_url, validate_custom_api_key_env_name,
};
use nca_common::message::{ContentPart, Message, MessageContent, Role};
use nca_common::tool::ToolDefinition;
use reqwest::RequestBuilder;
use reqwest::header::HeaderValue;
use serde_json::Value;
use std::time::Duration;

use super::anthropic_compat::{anthropic_request_body, spawn_anthropic_stream};
use super::openai_compat::{openai_request_body, spawn_openai_stream};
use super::{Provider, ProviderError, StreamChunk};

trait CustomProtocolAdapter: Sync {
    fn protocol_name(&self) -> &'static str;

    fn validate_api_key(&self, api_key: &str) -> Result<(), ProviderError>;

    fn probe_request(
        &self,
        client: &reqwest::Client,
        base_url: &str,
        config: &CustomProviderConfig,
        api_key: &str,
    ) -> Result<RequestBuilder, ProviderError>;

    fn validate_probe_response(&self, body: &str) -> Result<(), String>;

    fn chat_request(
        &self,
        client: &reqwest::Client,
        base_url: &str,
        body: Value,
        api_key: &str,
    ) -> Result<RequestBuilder, ProviderError>;

    #[allow(clippy::too_many_arguments)]
    fn chat_body(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        model: &str,
        max_tokens: u32,
        temperature: f32,
        reasoning_effort: &str,
        workspace_root: &Path,
    ) -> Result<Value, ProviderError>;

    fn spawn_stream(&self, response: reqwest::Response)
    -> tokio::sync::mpsc::Receiver<StreamChunk>;

    fn models_request(
        &self,
        client: &reqwest::Client,
        base_url: &str,
        api_key: &str,
        cursor: Option<&str>,
    ) -> Result<RequestBuilder, ProviderError>;

    fn parse_models_page(&self, body: &str) -> Result<CustomModelsPage, String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomModelCatalogEntry {
    pub id: String,
    pub context_window: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomModelsPage {
    pub models: Vec<CustomModelCatalogEntry>,
    pub has_more: bool,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustomCapabilityAdapter {
    OpenAi,
    Anthropic,
}

impl CustomCapabilityAdapter {
    pub fn from_compatibility(compatibility: ProviderCompatibility) -> Self {
        match compatibility {
            ProviderCompatibility::OpenAi | ProviderCompatibility::OpenAiResponses => Self::OpenAi,
            ProviderCompatibility::Anthropic => Self::Anthropic,
        }
    }

    fn protocol_adapter(self) -> &'static dyn CustomProtocolAdapter {
        match self {
            Self::OpenAi => &OPENAI_CUSTOM_ADAPTER,
            Self::Anthropic => &ANTHROPIC_CUSTOM_ADAPTER,
        }
    }

    pub fn models_request(
        self,
        client: &reqwest::Client,
        base_url: &str,
        api_key: &str,
        cursor: Option<&str>,
    ) -> Result<RequestBuilder, ProviderError> {
        self.protocol_adapter()
            .models_request(client, base_url, api_key, cursor)
    }

    pub fn parse_models_page(self, body: &str) -> Result<CustomModelsPage, String> {
        self.protocol_adapter().parse_models_page(body)
    }
}

struct OpenAiCustomAdapter;

static OPENAI_CUSTOM_ADAPTER: OpenAiCustomAdapter = OpenAiCustomAdapter;

impl CustomProtocolAdapter for OpenAiCustomAdapter {
    fn protocol_name(&self) -> &'static str {
        "OpenAI-compatible"
    }

    fn validate_api_key(&self, api_key: &str) -> Result<(), ProviderError> {
        HeaderValue::from_str(&format!("Bearer {api_key}"))
            .map(|_| ())
            .map_err(|error| {
                ProviderError::Configuration(sanitize_provider_error(
                    &format!("failed to build Custom provider authorization header: {error}"),
                    api_key,
                ))
            })
    }

    fn probe_request(
        &self,
        client: &reqwest::Client,
        base_url: &str,
        _config: &CustomProviderConfig,
        api_key: &str,
    ) -> Result<RequestBuilder, ProviderError> {
        self.validate_api_key(api_key)?;
        Ok(client
            .get(custom_provider_endpoint(base_url, "models"))
            .bearer_auth(api_key))
    }

    fn validate_probe_response(&self, body: &str) -> Result<(), String> {
        let value: Value = serde_json::from_str(body)
            .map_err(|error| format!("response is not valid JSON ({error})"))?;
        let object = value
            .as_object()
            .ok_or_else(|| "response must be a JSON object".to_string())?;
        let data = object
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| "response must contain a data array".to_string())?;

        for (index, model) in data.iter().enumerate() {
            if !model.is_object() {
                return Err(format!("data[{index}] must be a JSON object"));
            }
            if model.get("id").and_then(Value::as_str).is_none() {
                return Err(format!("data[{index}] must contain a string id"));
            }
        }

        Ok(())
    }

    fn chat_request(
        &self,
        client: &reqwest::Client,
        base_url: &str,
        body: Value,
        api_key: &str,
    ) -> Result<RequestBuilder, ProviderError> {
        self.validate_api_key(api_key)?;
        Ok(client
            .post(custom_provider_endpoint(base_url, "chat/completions"))
            .bearer_auth(api_key)
            .json(&body))
    }

    fn chat_body(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        model: &str,
        max_tokens: u32,
        temperature: f32,
        reasoning_effort: &str,
        workspace_root: &Path,
    ) -> Result<Value, ProviderError> {
        openai_request_body(
            messages,
            tools,
            model,
            max_tokens,
            temperature,
            reasoning_effort,
            workspace_root,
        )
    }

    fn spawn_stream(
        &self,
        response: reqwest::Response,
    ) -> tokio::sync::mpsc::Receiver<StreamChunk> {
        spawn_openai_stream(response, "custom")
    }

    fn models_request(
        &self,
        client: &reqwest::Client,
        base_url: &str,
        api_key: &str,
        _cursor: Option<&str>,
    ) -> Result<RequestBuilder, ProviderError> {
        self.validate_api_key(api_key)?;
        Ok(client
            .get(custom_provider_endpoint(base_url, "models"))
            .bearer_auth(api_key))
    }

    fn parse_models_page(&self, body: &str) -> Result<CustomModelsPage, String> {
        let value: Value = serde_json::from_str(body)
            .map_err(|error| format!("response is not valid JSON ({error})"))?;
        let data = value
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| "response must contain a data array".to_string())?;
        let models: Vec<CustomModelCatalogEntry> = data
            .iter()
            .filter_map(|model| {
                let object = model.as_object()?;
                let id = object
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.trim().is_empty())?;
                Some(CustomModelCatalogEntry {
                    id: id.to_string(),
                    context_window: object.get("context_window").and_then(Value::as_u64),
                })
            })
            .collect();
        Ok(CustomModelsPage {
            models,
            has_more: false,
            next_cursor: None,
        })
    }
}

struct OpenAiResponsesCustomAdapter;

static OPENAI_RESPONSES_CUSTOM_ADAPTER: OpenAiResponsesCustomAdapter = OpenAiResponsesCustomAdapter;

impl CustomProtocolAdapter for OpenAiResponsesCustomAdapter {
    fn protocol_name(&self) -> &'static str {
        "OpenAI Responses"
    }

    fn validate_api_key(&self, api_key: &str) -> Result<(), ProviderError> {
        OPENAI_CUSTOM_ADAPTER.validate_api_key(api_key)
    }

    fn probe_request(
        &self,
        client: &reqwest::Client,
        base_url: &str,
        config: &CustomProviderConfig,
        api_key: &str,
    ) -> Result<RequestBuilder, ProviderError> {
        OPENAI_CUSTOM_ADAPTER.probe_request(client, base_url, config, api_key)
    }

    fn validate_probe_response(&self, body: &str) -> Result<(), String> {
        OPENAI_CUSTOM_ADAPTER.validate_probe_response(body)
    }

    fn chat_request(
        &self,
        client: &reqwest::Client,
        base_url: &str,
        body: Value,
        api_key: &str,
    ) -> Result<RequestBuilder, ProviderError> {
        self.validate_api_key(api_key)?;
        Ok(client
            .post(custom_provider_endpoint(base_url, "responses"))
            .bearer_auth(api_key)
            .json(&body))
    }

    fn chat_body(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        model: &str,
        max_tokens: u32,
        temperature: f32,
        reasoning_effort: &str,
        workspace_root: &Path,
    ) -> Result<Value, ProviderError> {
        responses_request_body(
            messages,
            tools,
            model,
            max_tokens,
            temperature,
            reasoning_effort,
            workspace_root,
        )
    }

    fn spawn_stream(
        &self,
        response: reqwest::Response,
    ) -> tokio::sync::mpsc::Receiver<StreamChunk> {
        spawn_responses_stream(response, "custom")
    }

    fn models_request(
        &self,
        client: &reqwest::Client,
        base_url: &str,
        api_key: &str,
        cursor: Option<&str>,
    ) -> Result<RequestBuilder, ProviderError> {
        OPENAI_CUSTOM_ADAPTER.models_request(client, base_url, api_key, cursor)
    }

    fn parse_models_page(&self, body: &str) -> Result<CustomModelsPage, String> {
        OPENAI_CUSTOM_ADAPTER.parse_models_page(body)
    }
}

struct AnthropicCustomAdapter;

static ANTHROPIC_CUSTOM_ADAPTER: AnthropicCustomAdapter = AnthropicCustomAdapter;

impl CustomProtocolAdapter for AnthropicCustomAdapter {
    fn protocol_name(&self) -> &'static str {
        "Anthropic-compatible"
    }

    fn validate_api_key(&self, api_key: &str) -> Result<(), ProviderError> {
        HeaderValue::from_str(api_key).map(|_| ()).map_err(|error| {
            ProviderError::Configuration(sanitize_provider_error(
                &format!("failed to build Custom provider x-api-key header: {error}"),
                api_key,
            ))
        })
    }

    fn probe_request(
        &self,
        client: &reqwest::Client,
        base_url: &str,
        config: &CustomProviderConfig,
        api_key: &str,
    ) -> Result<RequestBuilder, ProviderError> {
        self.validate_api_key(api_key)?;
        Ok(client
            .post(custom_provider_endpoint(base_url, "messages"))
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&serde_json::json!({
                "model": config.model,
                "max_tokens": 1,
                "messages": [{"role": "user", "content": "ping"}],
            })))
    }

    fn validate_probe_response(&self, body: &str) -> Result<(), String> {
        let value: Value = serde_json::from_str(body)
            .map_err(|error| format!("response is not valid JSON ({error})"))?;
        let object = value
            .as_object()
            .ok_or_else(|| "response must be a JSON object".to_string())?;

        if object.get("id").and_then(Value::as_str).is_none() {
            return Err("response must contain a string id".to_string());
        }
        if object.get("type").and_then(Value::as_str) != Some("message") {
            return Err("response type must be message".to_string());
        }
        if object.get("role").and_then(Value::as_str) != Some("assistant") {
            return Err("response role must be assistant".to_string());
        }
        if object.get("model").and_then(Value::as_str).is_none() {
            return Err("response must contain a string model".to_string());
        }
        if !object.get("content").is_some_and(Value::is_array) {
            return Err("response must contain a content array".to_string());
        }
        if !object
            .get("stop_reason")
            .is_some_and(|value| value.is_null() || value.is_string())
        {
            return Err("response must contain a nullable stop_reason".to_string());
        }
        if !object
            .get("stop_sequence")
            .is_some_and(|value| value.is_null() || value.is_string())
        {
            return Err("response must contain a nullable stop_sequence".to_string());
        }

        let usage = object
            .get("usage")
            .and_then(Value::as_object)
            .ok_or_else(|| "response must contain a usage object".to_string())?;
        if usage.get("input_tokens").and_then(Value::as_u64).is_none()
            || usage.get("output_tokens").and_then(Value::as_u64).is_none()
        {
            return Err("usage must contain numeric input_tokens and output_tokens".to_string());
        }

        Ok(())
    }

    fn chat_request(
        &self,
        client: &reqwest::Client,
        base_url: &str,
        body: Value,
        api_key: &str,
    ) -> Result<RequestBuilder, ProviderError> {
        self.validate_api_key(api_key)?;
        Ok(client
            .post(custom_provider_endpoint(base_url, "messages"))
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&body))
    }

    fn chat_body(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        model: &str,
        max_tokens: u32,
        temperature: f32,
        _reasoning_effort: &str,
        workspace_root: &Path,
    ) -> Result<Value, ProviderError> {
        anthropic_request_body(
            messages,
            tools,
            model,
            max_tokens,
            temperature,
            workspace_root,
        )
    }

    fn spawn_stream(
        &self,
        response: reqwest::Response,
    ) -> tokio::sync::mpsc::Receiver<StreamChunk> {
        spawn_anthropic_stream(response, "custom")
    }

    fn models_request(
        &self,
        client: &reqwest::Client,
        base_url: &str,
        api_key: &str,
        cursor: Option<&str>,
    ) -> Result<RequestBuilder, ProviderError> {
        self.validate_api_key(api_key)?;
        let mut url = format!("{}?limit=100", custom_provider_endpoint(base_url, "models"));
        if let Some(cursor) = cursor {
            url.push_str("&after_id=");
            url.push_str(cursor);
        }
        Ok(client
            .get(url)
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01"))
    }

    fn parse_models_page(&self, body: &str) -> Result<CustomModelsPage, String> {
        let value: Value = serde_json::from_str(body)
            .map_err(|error| format!("response is not valid JSON ({error})"))?;
        let data = value
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| "response must contain a data array".to_string())?;
        let models: Vec<CustomModelCatalogEntry> = data
            .iter()
            .filter_map(|model| {
                let object = model.as_object()?;
                let id = object
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.trim().is_empty())?;
                Some(CustomModelCatalogEntry {
                    id: id.to_string(),
                    context_window: object.get("max_input_tokens").and_then(Value::as_u64),
                })
            })
            .collect();
        let has_more = value
            .get("has_more")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let next_cursor = models.last().map(|model| model.id.clone());
        Ok(CustomModelsPage {
            models,
            has_more,
            next_cursor,
        })
    }
}

fn responses_request_body(
    messages: &[Message],
    tools: &[ToolDefinition],
    model: &str,
    max_tokens: u32,
    temperature: f32,
    reasoning_effort: &str,
    workspace_root: &Path,
) -> Result<Value, ProviderError> {
    let mut input = Vec::new();
    for message in messages {
        match message.role {
            Role::System => input.push(serde_json::json!({
                "role": "system",
                "content": message_content_text(&message.content),
            })),
            Role::User => input.push(serde_json::json!({
                "role": "user",
                "content": responses_content_value(&message.content, workspace_root)?,
            })),
            Role::Assistant => {
                if !message.content.is_empty() {
                    input.push(serde_json::json!({
                        "role": "assistant",
                        "content": message_content_text(&message.content),
                    }));
                }
                if let Some(calls) = &message.tool_calls {
                    for call in calls {
                        input.push(serde_json::json!({
                            "type": "function_call",
                            "call_id": call.id,
                            "name": call.name,
                            "arguments": serde_json::to_string(&call.arguments)
                                .unwrap_or_else(|_| "{}".into()),
                        }));
                    }
                }
            }
            Role::Tool => input.push(serde_json::json!({
                "type": "function_call_output",
                "call_id": message.tool_call_id,
                "output": message_content_text(&message.content),
            })),
        }
    }

    let mut body = serde_json::json!({
        "model": model,
        "input": input,
        "stream": true,
        "store": false,
        "max_output_tokens": max_tokens,
        "temperature": temperature,
    });

    if !tools.is_empty() {
        body["tools"] = Value::Array(
            tools
                .iter()
                .map(|tool| {
                    serde_json::json!({
                        "type": "function",
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.parameters,
                    })
                })
                .collect(),
        );
    }

    let reasoning_effort = reasoning_effort.trim();
    if !reasoning_effort.is_empty() && reasoning_effort != "nil" {
        body["reasoning"] = serde_json::json!({"effort": reasoning_effort});
    }

    Ok(body)
}

fn message_content_text(content: &MessageContent) -> String {
    match content {
        MessageContent::Text(text) => text.clone(),
        MessageContent::Parts(_) => content.to_summary_text(),
    }
}

fn responses_content_value(
    content: &MessageContent,
    workspace_root: &Path,
) -> Result<Value, ProviderError> {
    match content {
        MessageContent::Text(text) => Ok(Value::String(text.clone())),
        MessageContent::Parts(parts) => {
            let mut blocks = Vec::new();
            for part in parts {
                match part {
                    ContentPart::Text { text } => blocks.push(serde_json::json!({
                        "type": "input_text",
                        "text": text,
                    })),
                    ContentPart::Image { media_type, path } => {
                        let media_type = media_type.trim().to_ascii_lowercase();
                        if !matches!(
                            media_type.as_str(),
                            "image/png" | "image/jpeg" | "image/webp" | "image/gif"
                        ) {
                            return Err(ProviderError::RequestFailed(format!(
                                "unsupported image media type `{media_type}` for Responses input; supported types are image/png, image/jpeg, image/webp, and image/gif"
                            )));
                        }
                        let full_path = workspace_root.join(path);
                        let bytes = std::fs::read(&full_path).map_err(|error| {
                            ProviderError::RequestFailed(format!(
                                "failed to read image {}: {error}",
                                full_path.display()
                            ))
                        })?;
                        blocks.push(serde_json::json!({
                            "type": "input_image",
                            "image_url": format!(
                                "data:{media_type};base64,{}",
                                B64.encode(bytes)
                            ),
                        }));
                    }
                }
            }
            Ok(Value::Array(blocks))
        }
    }
}

#[derive(Default)]
struct ResponsesToolAccumulator {
    call_id: String,
    name: String,
    arguments: String,
}

#[derive(Default)]
struct ResponsesToolState {
    tools: BTreeMap<String, ResponsesToolAccumulator>,
    aliases: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
struct ResponsesIdentityCandidate {
    key: String,
    value: String,
}

impl ResponsesToolState {
    fn resolve(
        &mut self,
        candidates: Vec<ResponsesIdentityCandidate>,
    ) -> Result<String, &'static str> {
        let Some(canonical) = candidates
            .iter()
            .find_map(|candidate| self.aliases.get(&candidate.key))
            .cloned()
            .or_else(|| candidates.first().map(|candidate| candidate.key.clone()))
        else {
            return Err(
                "function call is missing a stable identity (call_id, item id, or output_index)",
            );
        };

        let fallback_call_id = candidates
            .first()
            .map(|candidate| candidate.value.clone())
            .unwrap_or_default();
        for candidate in candidates {
            self.aliases.insert(candidate.key, canonical.clone());
        }
        self.tools
            .entry(canonical.clone())
            .or_insert_with(|| ResponsesToolAccumulator {
                call_id: fallback_call_id,
                ..ResponsesToolAccumulator::default()
            });
        Ok(canonical)
    }
}

fn responses_identity_candidates(
    value: &Value,
    item: Option<&serde_json::Map<String, Value>>,
) -> Vec<ResponsesIdentityCandidate> {
    let mut candidates = Vec::new();
    let mut add = |kind: &str, value: Option<&Value>| {
        let Some(value) = value else {
            return;
        };
        let value = match value {
            Value::String(value) if !value.trim().is_empty() => value.to_string(),
            Value::Number(value) => value.to_string(),
            _ => return,
        };
        candidates.push(ResponsesIdentityCandidate {
            key: format!("{kind}:{value}"),
            value,
        });
    };

    add(
        "call_id",
        item.and_then(|item| item.get("call_id"))
            .or_else(|| value.get("call_id")),
    );
    add("item_id", item.and_then(|item| item.get("id")));
    add("item_id", value.get("item_id"));
    add("output_index", value.get("output_index"));
    candidates
}

fn explicit_responses_call_id(
    value: &Value,
    item: Option<&serde_json::Map<String, Value>>,
) -> Option<String> {
    item.and_then(|item| item.get("call_id"))
        .or_else(|| value.get("call_id"))
        .and_then(Value::as_str)
        .filter(|call_id| !call_id.trim().is_empty())
        .map(str::to_string)
}

fn spawn_responses_stream(
    response: reqwest::Response,
    provider_name: &'static str,
) -> tokio::sync::mpsc::Receiver<StreamChunk> {
    use futures_util::StreamExt;

    let mut byte_stream = response.bytes_stream();
    let (tx, rx) = tokio::sync::mpsc::channel(64);

    tokio::spawn(async move {
        let mut buffer = Vec::new();
        let mut event_name = String::new();
        let mut tools = ResponsesToolState::default();
        let mut emitted_tools = BTreeSet::new();
        let mut produced_output = false;
        let mut completed = false;

        while let Some(item) = byte_stream.next().await {
            let chunk = match item {
                Ok(chunk) => chunk,
                Err(error) => {
                    let _ = tx
                        .send(StreamChunk::Error(format!(
                            "{provider_name} Responses stream error: {error}"
                        )))
                        .await;
                    return;
                }
            };
            buffer.extend_from_slice(&chunk);

            loop {
                let raw = match take_responses_sse_line(&mut buffer) {
                    Ok(Some(line)) => line,
                    Ok(None) => break,
                    Err(error) => {
                        let _ = tx
                            .send(StreamChunk::Error(format!(
                                "{provider_name} Responses stream error: {error}"
                            )))
                            .await;
                        return;
                    }
                };
                let line = raw.trim_end_matches('\r').trim();
                if line.is_empty() {
                    continue;
                }
                if let Some(name) = line.strip_prefix("event:") {
                    event_name = name.trim().to_string();
                    continue;
                }
                let Some(data) = line.strip_prefix("data:").map(str::trim) else {
                    continue;
                };
                if data == "[DONE]" {
                    completed = true;
                    break;
                }

                let value = match serde_json::from_str::<Value>(data) {
                    Ok(value) => value,
                    Err(error) => {
                        if is_known_responses_event(&event_name) {
                            let _ = tx
                                .send(StreamChunk::Error(format!(
                                    "{provider_name} Responses event is malformed: {error}"
                                )))
                                .await;
                            return;
                        }
                        continue;
                    }
                };
                let event_type = value
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or(event_name.as_str());
                match event_type {
                    "response.output_text.delta" => {
                        let Some(delta) = value.get("delta").and_then(Value::as_str) else {
                            send_responses_error(
                                &tx,
                                provider_name,
                                "text delta is missing a string delta",
                            )
                            .await;
                            return;
                        };
                        if !delta.is_empty() {
                            let _ = tx.send(StreamChunk::TextDelta(delta.to_string())).await;
                            produced_output = true;
                        }
                    }
                    "response.output_item.added" | "response.output_item.done" => {
                        let Some(item) = value.get("item").and_then(Value::as_object) else {
                            send_responses_error(
                                &tx,
                                provider_name,
                                "function output item is missing",
                            )
                            .await;
                            return;
                        };
                        if item.get("type").and_then(Value::as_str) == Some("function_call") {
                            let key = match tools
                                .resolve(responses_identity_candidates(&value, Some(item)))
                            {
                                Ok(key) => key,
                                Err(message) => {
                                    send_responses_error(&tx, provider_name, message).await;
                                    return;
                                }
                            };
                            let entry = tools.tools.get_mut(&key).expect("resolved tool entry");
                            if let Some(call_id) = explicit_responses_call_id(&value, Some(item)) {
                                entry.call_id = call_id;
                            }
                            if let Some(name) = item.get("name").and_then(Value::as_str) {
                                entry.name = name.to_string();
                            }
                            if let Some(arguments) = item.get("arguments").and_then(Value::as_str) {
                                entry.arguments = arguments.to_string();
                            }
                            if event_type == "response.output_item.done" {
                                let emitted = match emit_responses_tool(
                                    &tx,
                                    provider_name,
                                    &tools.tools,
                                    &mut emitted_tools,
                                    &key,
                                )
                                .await
                                {
                                    Ok(emitted) => emitted,
                                    Err(()) => return,
                                };
                                produced_output = emitted || emitted_tools.contains(&key);
                            }
                        }
                    }
                    "response.function_call_arguments.delta" => {
                        let Some(delta) = value.get("delta").and_then(Value::as_str) else {
                            send_responses_error(
                                &tx,
                                provider_name,
                                "function arguments delta is missing a string delta",
                            )
                            .await;
                            return;
                        };
                        let key = match tools.resolve(responses_identity_candidates(&value, None)) {
                            Ok(key) => key,
                            Err(message) => {
                                send_responses_error(&tx, provider_name, message).await;
                                return;
                            }
                        };
                        let entry = tools.tools.get_mut(&key).expect("resolved tool entry");
                        entry.arguments.push_str(delta);
                    }
                    "response.function_call_arguments.done" => {
                        let key = match tools.resolve(responses_identity_candidates(&value, None)) {
                            Ok(key) => key,
                            Err(message) => {
                                send_responses_error(&tx, provider_name, message).await;
                                return;
                            }
                        };
                        if let Some(arguments) = value.get("arguments").and_then(Value::as_str) {
                            tools
                                .tools
                                .get_mut(&key)
                                .expect("resolved tool entry")
                                .arguments = arguments.into();
                        }
                        let emitted = match emit_responses_tool(
                            &tx,
                            provider_name,
                            &tools.tools,
                            &mut emitted_tools,
                            &key,
                        )
                        .await
                        {
                            Ok(emitted) => emitted,
                            Err(()) => return,
                        };
                        produced_output = emitted || emitted_tools.contains(&key);
                    }
                    "response.completed" => {
                        if tools.tools.keys().any(|key| !emitted_tools.contains(key)) {
                            send_responses_error(
                                &tx,
                                provider_name,
                                "function call output is incomplete",
                            )
                            .await;
                            return;
                        }
                        if let Some(usage) = value
                            .get("response")
                            .and_then(|response| response.get("usage"))
                            .and_then(Value::as_object)
                        {
                            let input_tokens = usage
                                .get("input_tokens")
                                .and_then(Value::as_u64)
                                .unwrap_or(0);
                            let output_tokens = usage
                                .get("output_tokens")
                                .and_then(Value::as_u64)
                                .unwrap_or(0);
                            if input_tokens > 0 || output_tokens > 0 {
                                let _ = tx
                                    .send(StreamChunk::Usage {
                                        input_tokens,
                                        output_tokens,
                                    })
                                    .await;
                            }
                        }
                        completed = true;
                        break;
                    }
                    "response.failed" => {
                        let message = value
                            .get("response")
                            .and_then(|response| response.get("error"))
                            .and_then(|error| error.get("message"))
                            .and_then(Value::as_str)
                            .unwrap_or("provider reported a failed response");
                        send_responses_error(&tx, provider_name, message).await;
                        return;
                    }
                    _ => {}
                }
                event_name.clear();
                if completed {
                    break;
                }
            }
        }

        if let Err(error) = std::str::from_utf8(&buffer) {
            let _ = tx
                .send(StreamChunk::Error(format!(
                    "{provider_name} Responses stream error: invalid UTF-8 in incomplete SSE line: {error}"
                )))
                .await;
            return;
        }

        if tools.tools.keys().any(|key| !emitted_tools.contains(key)) {
            send_responses_error(&tx, provider_name, "function call output is incomplete").await;
            return;
        }

        finish_responses_stream(&tx, provider_name, produced_output).await;
    });

    rx
}

fn take_responses_sse_line(buffer: &mut Vec<u8>) -> Result<Option<String>, String> {
    let Some(newline) = buffer.iter().position(|byte| *byte == b'\n') else {
        return Ok(None);
    };

    let line = buffer.drain(..newline).collect::<Vec<_>>();
    buffer.drain(..1);
    String::from_utf8(line)
        .map(Some)
        .map_err(|error| format!("invalid UTF-8 in SSE line: {error}"))
}

fn is_known_responses_event(event_name: &str) -> bool {
    matches!(
        event_name,
        "response.output_text.delta"
            | "response.output_item.added"
            | "response.output_item.done"
            | "response.function_call_arguments.delta"
            | "response.function_call_arguments.done"
            | "response.completed"
            | "response.failed"
    )
}

async fn emit_responses_tool(
    tx: &tokio::sync::mpsc::Sender<StreamChunk>,
    provider_name: &'static str,
    tools: &BTreeMap<String, ResponsesToolAccumulator>,
    emitted: &mut BTreeSet<String>,
    key: &str,
) -> Result<bool, ()> {
    if emitted.contains(key) {
        return Ok(false);
    }
    let Some(tool) = tools.get(key) else {
        send_responses_error(tx, provider_name, "function call item is missing").await;
        return Err(());
    };
    if tool.call_id.is_empty() {
        send_responses_error(
            tx,
            provider_name,
            "function call is missing a stable identity",
        )
        .await;
        return Err(());
    }
    if tool.name.trim().is_empty() {
        send_responses_error(tx, provider_name, "function call is missing its name").await;
        return Err(());
    }
    let Ok(input) = serde_json::from_str(&tool.arguments) else {
        send_responses_error(
            tx,
            provider_name,
            "function call arguments are invalid JSON",
        )
        .await;
        return Err(());
    };
    let _ = tx
        .send(StreamChunk::ToolUse(nca_common::tool::ToolCall {
            id: tool.call_id.clone(),
            name: tool.name.clone(),
            input,
        }))
        .await;
    emitted.insert(key.to_string());
    Ok(true)
}

async fn send_responses_error(
    tx: &tokio::sync::mpsc::Sender<StreamChunk>,
    provider_name: &'static str,
    message: &str,
) {
    let _ = tx
        .send(StreamChunk::Error(format!(
            "{provider_name} Responses provider error: {message}"
        )))
        .await;
}

async fn finish_responses_stream(
    tx: &tokio::sync::mpsc::Sender<StreamChunk>,
    provider_name: &'static str,
    produced_output: bool,
) {
    if produced_output {
        let _ = tx.send(StreamChunk::Done).await;
    } else {
        let _ = tx
            .send(StreamChunk::Error(format!(
                "{provider_name} Responses provider returned an empty completion"
            )))
            .await;
    }
}

fn custom_adapter(compatibility: ProviderCompatibility) -> &'static dyn CustomProtocolAdapter {
    match compatibility {
        ProviderCompatibility::OpenAi => &OPENAI_CUSTOM_ADAPTER,
        ProviderCompatibility::Anthropic => &ANTHROPIC_CUSTOM_ADAPTER,
        ProviderCompatibility::OpenAiResponses => &OPENAI_RESPONSES_CUSTOM_ADAPTER,
    }
}

pub struct CustomProvider {
    client: reqwest::Client,
    config: CustomProviderConfig,
    max_tokens: u32,
    reasoning_effort: String,
    debug_requests: bool,
    debug_log_path: PathBuf,
}

impl CustomProvider {
    pub fn from_config(config: &NcaConfig) -> Result<Self, ProviderError> {
        let mut custom = config.provider.custom.clone();
        custom.base_url =
            normalize_custom_provider_base_url(&custom.base_url).map_err(custom_config_error)?;
        validate_custom_api_key_env_name(&custom.api_key_env).map_err(custom_config_error)?;
        let api_key = custom.resolve_api_key().ok_or_else(|| {
            ProviderError::Configuration(format!(
                "missing Custom provider API key; set {} or provide `provider.custom.api_key` in config",
                custom.api_key_env
            ))
        })?;
        if custom.base_url.trim().is_empty() {
            return Err(ProviderError::Configuration(
                "missing Custom provider base URL; set `provider.custom.base_url`".into(),
            ));
        }

        custom_adapter(custom.compatibility).validate_api_key(&api_key)?;

        let client = reqwest::Client::builder().build().map_err(|err| {
            ProviderError::Configuration(format!("failed to build HTTP client: {err}"))
        })?;

        Ok(Self {
            client,
            config: custom,
            max_tokens: config.model.max_tokens,
            reasoning_effort: config.model.reasoning_effort.clone(),
            debug_requests: request_debug_enabled(),
            debug_log_path: PathBuf::from("debug.log"),
        })
    }

    #[cfg(test)]
    fn with_debug_log_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.debug_log_path = path.into();
        self
    }

    #[cfg(test)]
    fn with_debug_requests(mut self, enabled: bool) -> Self {
        self.debug_requests = enabled;
        self
    }

    fn emit_debug_request(&self, request: &reqwest::Request, api_key: &str, protocol: &str) {
        let output = format_debug_request(request, api_key, protocol);
        if let Err(error) = append_debug_log(&self.debug_log_path, &output) {
            eprintln!(
                "nca: failed to write custom provider request diagnostics to {}: {error}",
                self.debug_log_path.display()
            );
        }
    }
}

fn custom_config_error(error: CustomProviderConfigError) -> ProviderError {
    ProviderError::Configuration(error.to_string())
}

/// Perform the cheap protocol-specific reachability check used by custom
/// provider setup before activation.
pub async fn probe_custom_provider(config: &CustomProviderConfig) -> Result<(), ProviderError> {
    let base_url =
        normalize_custom_provider_base_url(&config.base_url).map_err(custom_config_error)?;
    validate_custom_api_key_env_name(&config.api_key_env).map_err(custom_config_error)?;
    let api_key = config.resolve_api_key().ok_or_else(|| {
        ProviderError::Configuration("custom provider API key is required for probing".into())
    })?;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| {
            ProviderError::Configuration(format!("failed to build probe client: {error}"))
        })?;
    let adapter = custom_adapter(config.compatibility);
    let response = adapter
        .probe_request(&client, &base_url, config, &api_key)?
        .send()
        .await
        .map_err(|error| request_failed(&error.to_string(), &api_key))?;

    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| request_failed(&error.to_string(), &api_key))?;
    if status.is_success() {
        return adapter.validate_probe_response(&body).map_err(|reason| {
            malformed_probe_error(adapter.protocol_name(), &body, reason, &api_key)
        });
    }

    Err(map_custom_provider_error(status, body, &api_key))
}

fn request_failed(message: &str, api_key: &str) -> ProviderError {
    ProviderError::RequestFailed(sanitize_provider_error(message, api_key))
}

fn malformed_probe_error(
    protocol: &str,
    body: &str,
    reason: String,
    api_key: &str,
) -> ProviderError {
    let body = sanitize_provider_error(body, api_key);
    ProviderError::RequestFailed(format!(
        "custom {protocol} probe returned a malformed successful response: {reason}; body: {body}"
    ))
}

fn map_custom_provider_error(
    status: reqwest::StatusCode,
    body: String,
    api_key: &str,
) -> ProviderError {
    let body = sanitize_provider_error(&body, api_key);
    match status.as_u16() {
        401 | 403 => ProviderError::AuthError(body),
        404 => ProviderError::ModelNotFound(body),
        429 => ProviderError::RateLimited {
            retry_after_ms: 1000,
        },
        _ => ProviderError::RequestFailed(body),
    }
}

fn sanitize_provider_error(body: &str, api_key: &str) -> String {
    let body = redact_debug_value(body, api_key);
    let body = body.trim();
    if body.is_empty() {
        return "custom provider returned an empty error response".into();
    }
    body.chars().take(512).collect()
}

fn debug_request_enabled(value: Option<&str>) -> bool {
    value == Some("1")
}

fn request_debug_enabled() -> bool {
    debug_request_enabled(std::env::var("NCA_DEBUG_REQUEST").ok().as_deref())
}

fn redact_debug_value(value: &str, api_key: &str) -> String {
    if api_key.is_empty() {
        value.to_string()
    } else {
        value.replace(api_key, "[REDACTED]")
    }
}

fn append_debug_log(path: &Path, record: &str) -> std::io::Result<()> {
    let mut file = open_debug_log(path)?;
    file.write_all(record.as_bytes())
}

fn open_debug_log(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn format_debug_request(request: &reqwest::Request, api_key: &str, protocol: &str) -> String {
    let url = redact_debug_value(request.url().as_str(), api_key);
    let mut output = format!(
        "[{}] custom {protocol} request\nmethod: {}\nurl: {url}\nheaders:\n",
        chrono::Utc::now().to_rfc3339(),
        request.method()
    );

    for (name, value) in request.headers() {
        let value = if name.as_str().eq_ignore_ascii_case("authorization")
            || name.as_str().eq_ignore_ascii_case("x-api-key")
        {
            "[REDACTED]".to_string()
        } else {
            redact_debug_value(value.to_str().unwrap_or("[INVALID HEADER]"), api_key)
        };
        output.push_str(&format!("  {name}: {value}\n"));
    }

    if let Some(body) = request.body().and_then(reqwest::Body::as_bytes) {
        output.push_str("body:\n");
        output.push_str(&format_debug_body(body, api_key));
        output.push('\n');
    }

    output.push('\n');
    output
}

fn format_debug_body(body: &[u8], api_key: &str) -> String {
    let body = String::from_utf8_lossy(body);
    match serde_json::from_str::<Value>(&body) {
        Ok(mut value) => {
            redact_debug_json_value(&mut value, api_key);
            serde_json::to_string_pretty(&value)
                .map(|pretty| redact_debug_value(&pretty, api_key))
                .unwrap_or_else(|_| redact_debug_value(&body, api_key))
        }
        Err(_) => redact_debug_value(&body, api_key),
    }
}

fn redact_debug_json_value(value: &mut Value, api_key: &str) {
    match value {
        Value::String(value) => *value = redact_debug_value(value, api_key),
        Value::Array(values) => {
            for value in values {
                redact_debug_json_value(value, api_key);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                redact_debug_json_value(value, api_key);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

#[async_trait::async_trait]
impl Provider for CustomProvider {
    async fn chat(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        model: &str,
        workspace_root: &Path,
    ) -> Result<tokio::sync::mpsc::Receiver<StreamChunk>, ProviderError> {
        let api_key = self.config.resolve_api_key().ok_or_else(|| {
            ProviderError::Configuration(format!(
                "missing Custom provider API key; set {} or provide an inline key",
                self.config.api_key_env
            ))
        })?;
        let model = if model.is_empty() {
            self.config.model.clone()
        } else {
            model.to_string()
        };

        let adapter = custom_adapter(self.config.compatibility);
        let body = adapter.chat_body(
            messages,
            tools,
            &model,
            self.max_tokens,
            self.config.temperature,
            &self.reasoning_effort,
            workspace_root,
        )?;
        let request = adapter
            .chat_request(&self.client, &self.config.base_url, body, &api_key)?
            .build()
            .map_err(|error| request_failed(&error.to_string(), &api_key))?;

        if self.debug_requests
            && matches!(
                self.config.compatibility,
                ProviderCompatibility::OpenAi | ProviderCompatibility::OpenAiResponses
            )
        {
            self.emit_debug_request(&request, &api_key, adapter.protocol_name());
        }

        let response = self
            .client
            .execute(request)
            .await
            .map_err(|error| request_failed(&error.to_string(), &api_key))?;

        let status = response.status();
        if !status.is_success() {
            let body_text = response
                .text()
                .await
                .map_err(|error| request_failed(&error.to_string(), &api_key))?;
            return Err(map_custom_provider_error(status, body_text, &api_key));
        }

        Ok(adapter.spawn_stream(response))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::test_support::{collect_chunks, spawn_sse_server};
    use serde_json::json;
    use std::io::Read;
    use std::sync::mpsc;
    use tiny_http::{Header, Request, Response, Server, StatusCode};

    #[tokio::test]
    async fn openai_probe_lists_models_with_bearer_auth() {
        let (base_url, request_rx) =
            spawn_probe_server(200, r#"{"data":[{"id":"gateway-model"}]}"#);
        let config = custom_config(ProviderCompatibility::OpenAi, &base_url, "probe-secret");

        probe_custom_provider(&config)
            .await
            .expect("probe succeeds");

        let request = request_rx.recv().expect("probe request");
        assert_eq!(request.method, "GET");
        assert_eq!(request.url, "/v1/models");
        assert_eq!(
            request.authorization.as_deref(),
            Some("Bearer probe-secret")
        );
    }

    #[tokio::test]
    async fn openai_probe_accepts_an_empty_model_collection() {
        let (base_url, request_rx) = spawn_probe_server(200, r#"{"data":[]}"#);
        let config = custom_config(ProviderCompatibility::OpenAi, &base_url, "probe-secret");

        probe_custom_provider(&config)
            .await
            .expect("empty model collection is a valid probe response");

        let request = request_rx.recv().expect("probe request");
        assert_eq!(request.method, "GET");
        assert_eq!(request.url, "/v1/models");
    }

    #[tokio::test]
    async fn anthropic_probe_preserves_versioned_path_prefix_and_sends_required_headers() {
        let (base_url, request_rx) = spawn_probe_server(200, valid_anthropic_probe_response());
        let prefixed_base_url = format!("{base_url}/zen/v1");
        let config = custom_config(
            ProviderCompatibility::Anthropic,
            &prefixed_base_url,
            "probe-secret",
        );

        probe_custom_provider(&config)
            .await
            .expect("probe succeeds");

        let request = request_rx.recv().expect("probe request");
        assert_eq!(request.method, "POST");
        assert_eq!(request.url, "/zen/v1/messages");
        assert_eq!(request.x_api_key.as_deref(), Some("probe-secret"));
        assert_eq!(request.anthropic_version.as_deref(), Some("2023-06-01"));
        assert!(request.body.contains("\"messages\""));
        assert!(request.body.contains("\"max_tokens\""));
    }

    #[tokio::test]
    async fn successful_probe_bodies_must_match_the_selected_protocol() {
        let cases = [
            (
                ProviderCompatibility::OpenAi,
                "<html>gateway probe-secret failure</html>",
            ),
            (ProviderCompatibility::OpenAi, r#"{"data":"not-an-array"}"#),
            (ProviderCompatibility::OpenAi, r#"{"data":"#),
            (ProviderCompatibility::OpenAi, r#"{"type":"message"}"#),
            (
                ProviderCompatibility::Anthropic,
                "<html>gateway probe-secret failure</html>",
            ),
            (ProviderCompatibility::Anthropic, r#"{"type":"message"#),
            (ProviderCompatibility::Anthropic, r#"{"type":"message"}"#),
            (ProviderCompatibility::Anthropic, r#"{"data":[]}"#),
        ];

        for (compatibility, response_body) in cases {
            let (base_url, request_rx) = spawn_probe_server(200, response_body);
            let config = custom_config(compatibility, &base_url, "probe-secret");
            let error = probe_custom_provider(&config)
                .await
                .expect_err("malformed successful response is rejected");
            let message = error.to_string();

            assert!(
                matches!(&error, ProviderError::RequestFailed(_)),
                "expected retryable failure, got {error:?}"
            );
            assert!(message.contains("malformed successful response"));
            assert!(!message.contains("probe-secret"));
            let _ = request_rx.recv().expect("probe request");
        }
    }

    #[tokio::test]
    async fn probe_errors_redact_credentials_but_keep_safe_diagnostic() {
        let (base_url, request_rx) = spawn_probe_server(
            502,
            r#"upstream rejected probe-secret while contacting gateway"#,
        );
        let config = custom_config(ProviderCompatibility::OpenAi, &base_url, "probe-secret");

        let error = probe_custom_provider(&config)
            .await
            .expect_err("probe fails");
        let message = error.to_string();
        let _ = request_rx.recv();

        assert!(!message.contains("probe-secret"));
        assert!(message.contains("upstream rejected"));
    }

    #[tokio::test]
    async fn probe_requires_a_resolvable_credential() {
        let config = CustomProviderConfig {
            api_key_env: "__NCA_CUSTOM_PROBE_KEY_MISSING__".into(),
            api_key: None,
            base_url: "https://gateway.example".into(),
            model: "gateway-model".into(),
            temperature: 0.7,
            compatibility: ProviderCompatibility::OpenAi,
        };

        let error = probe_custom_provider(&config)
            .await
            .expect_err("missing credential blocks probing");

        assert!(
            matches!(error, ProviderError::Configuration(message) if message.contains("required"))
        );
    }

    #[tokio::test]
    async fn empty_openai_completion_is_reported_as_stream_error() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        )
        .to_string();
        let base_url = spawn_sse_server(body, 200, |_| {});
        let mut config = NcaConfig::default();
        config.provider.custom.api_key = Some("custom-test-key".into());
        config.provider.custom.base_url = base_url;

        let debug_dir = tempfile::tempdir().expect("debug directory");
        let debug_path = debug_dir.path().join("debug.log");
        let provider = CustomProvider::from_config(&config)
            .expect("provider")
            .with_debug_log_path(&debug_path)
            .with_debug_requests(true);
        let stream = provider
            .chat(&[Message::user("hello")], &[], "", Path::new("."))
            .await
            .expect("chat stream");
        let chunks = collect_chunks(stream).await;

        assert!(matches!(
            chunks.as_slice(),
            [StreamChunk::Error(message)] if message.contains("empty")
        ));
        let debug_output = std::fs::read_to_string(debug_path).expect("debug log");
        assert!(debug_output.contains("custom OpenAI-compatible request"));
        assert!(!debug_output.contains("empty completion"));
    }

    #[tokio::test]
    async fn request_debug_log_failure_does_not_block_custom_request() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}\n\n",
            "data: [DONE]\n\n"
        );
        let base_url = spawn_sse_server(body.to_string(), 200, |_| {});
        let mut config = NcaConfig::default();
        config.provider.custom.api_key = Some("custom-test-key".into());
        config.provider.custom.base_url = base_url;

        let debug_dir = tempfile::tempdir().expect("debug directory");
        let debug_path = debug_dir.path().join("missing").join("debug.log");
        let provider = CustomProvider::from_config(&config)
            .expect("provider")
            .with_debug_log_path(&debug_path)
            .with_debug_requests(true);

        let stream = provider
            .chat(&[Message::user("hello")], &[], "", Path::new("."))
            .await
            .expect("chat stream");
        let chunks = collect_chunks(stream).await;

        assert!(
            chunks
                .iter()
                .any(|chunk| matches!(chunk, StreamChunk::Done))
        );
        assert!(!debug_path.exists());
    }

    #[tokio::test]
    async fn empty_anthropic_completion_is_reported_as_stream_error() {
        let base_url = spawn_sse_server(String::new(), 200, |_| {});
        let mut config = NcaConfig::default();
        config.provider.custom.api_key = Some("custom-test-key".into());
        config.provider.custom.base_url = base_url;
        config.provider.custom.compatibility = ProviderCompatibility::Anthropic;

        let provider = CustomProvider::from_config(&config).expect("provider");
        let stream = provider
            .chat(&[Message::user("hello")], &[], "", Path::new("."))
            .await
            .expect("chat stream");
        let chunks = collect_chunks(stream).await;

        assert!(matches!(
            chunks.as_slice(),
            [StreamChunk::Error(message)] if message.contains("empty")
        ));
    }

    fn custom_config(
        compatibility: ProviderCompatibility,
        base_url: &str,
        api_key: &str,
    ) -> CustomProviderConfig {
        CustomProviderConfig {
            api_key_env: "CUSTOM_TEST_KEY".into(),
            api_key: Some(api_key.into()),
            base_url: base_url.into(),
            model: "gateway-model".into(),
            temperature: 0.7,
            compatibility,
        }
    }

    fn valid_anthropic_probe_response() -> &'static str {
        r#"{
            "id":"msg_1",
            "type":"message",
            "role":"assistant",
            "model":"gateway-model",
            "content":[{"type":"text","text":"pong"}],
            "stop_reason":"end_turn",
            "stop_sequence":null,
            "usage":{"input_tokens":1,"output_tokens":1}
        }"#
    }

    #[derive(Debug)]
    struct CapturedRequest {
        method: String,
        url: String,
        authorization: Option<String>,
        x_api_key: Option<String>,
        anthropic_version: Option<String>,
        body: String,
    }

    #[derive(Debug)]
    struct CapturedChatRequest {
        method: String,
        url: String,
        content_type: Option<String>,
        authorization: Option<String>,
        x_api_key: Option<String>,
        anthropic_version: Option<String>,
        body: String,
    }

    fn capture_chat_request(request: &mut Request) -> CapturedChatRequest {
        let mut body = String::new();
        request
            .as_reader()
            .read_to_string(&mut body)
            .expect("read chat request body");
        let header = |name: &str| {
            request
                .headers()
                .iter()
                .find(|header| header.field.as_str().as_str().eq_ignore_ascii_case(name))
                .map(|header| header.value.as_str().to_string())
        };
        CapturedChatRequest {
            method: request.method().as_str().to_string(),
            url: request.url().to_string(),
            content_type: header("content-type"),
            authorization: header("authorization"),
            x_api_key: header("x-api-key"),
            anthropic_version: header("anthropic-version"),
            body,
        }
    }

    fn spawn_chunked_sse_server(chunks: Vec<Vec<u8>>) -> String {
        let server = Server::http("127.0.0.1:0").expect("start chunked SSE server");
        let base_url = match server.server_addr() {
            tiny_http::ListenAddr::IP(addr) => format!("http://{addr}"),
            other => panic!("unsupported listen addr: {other:?}"),
        };

        std::thread::spawn(move || {
            let mut request = server.recv().expect("receive chunked SSE request");
            let mut request_body = Vec::new();
            request
                .as_reader()
                .read_to_end(&mut request_body)
                .expect("read chunked SSE request body");
            let response = Response::new(
                StatusCode(200),
                vec![Header::from_bytes("content-type", "text/event-stream").unwrap()],
                ChunkedBodyReader { chunks, index: 0 },
                None,
                None,
            );
            request
                .respond(response)
                .expect("send chunked SSE response");
        });

        base_url
    }

    struct ChunkedBodyReader {
        chunks: Vec<Vec<u8>>,
        index: usize,
    }

    impl Read for ChunkedBodyReader {
        fn read(&mut self, target: &mut [u8]) -> std::io::Result<usize> {
            let Some(chunk) = self.chunks.get(self.index) else {
                return Ok(0);
            };
            let length = chunk.len().min(target.len());
            target[..length].copy_from_slice(&chunk[..length]);
            if length == chunk.len() {
                self.index += 1;
            } else {
                self.chunks[self.index].drain(..length);
            }
            Ok(length)
        }
    }

    fn spawn_probe_server(
        status: u16,
        response_body: &str,
    ) -> (String, mpsc::Receiver<CapturedRequest>) {
        let server = Server::http("127.0.0.1:0").expect("start probe server");
        let base_url = match server.server_addr() {
            tiny_http::ListenAddr::IP(addr) => format!("http://{addr}"),
            other => panic!("unsupported listen addr: {other:?}"),
        };
        let (request_tx, request_rx) = mpsc::channel();
        let response_body = response_body.to_string();
        std::thread::spawn(move || {
            let mut request = server.recv().expect("receive probe request");
            let mut body = String::new();
            request
                .as_reader()
                .read_to_string(&mut body)
                .expect("read body");
            let header = |name: &str| {
                request
                    .headers()
                    .iter()
                    .find(|header| header.field.as_str().as_str().eq_ignore_ascii_case(name))
                    .map(|header| header.value.as_str().to_string())
            };
            request_tx
                .send(CapturedRequest {
                    method: request.method().as_str().to_string(),
                    url: request.url().to_string(),
                    authorization: header("authorization"),
                    x_api_key: header("x-api-key"),
                    anthropic_version: header("anthropic-version"),
                    body,
                })
                .expect("capture request");
            request
                .respond(
                    Response::from_string(response_body)
                        .with_status_code(StatusCode(status))
                        .with_header(
                            Header::from_bytes("content-type", "application/json").unwrap(),
                        ),
                )
                .expect("respond to probe");
        });
        (base_url, request_rx)
    }

    #[tokio::test]
    async fn custom_openai_compatible_provider_streams() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hello \"},\"index\":0,\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":2}}\n\n",
            "data: [DONE]\n\n"
        )
        .to_string();
        let (request_tx, request_rx) = mpsc::channel();
        let base_url = spawn_sse_server(body, 200, move |request| {
            request_tx
                .send(capture_chat_request(request))
                .expect("capture OpenAI chat request");
        });

        let mut config = NcaConfig::default();
        config.provider.custom.api_key = Some("custom-test-key".into());
        config.provider.custom.base_url = base_url;
        config.provider.custom.compatibility = ProviderCompatibility::OpenAi;
        config.provider.custom.model = "custom-openai-model".into();
        config.model.reasoning_effort = "  low  ".into();

        let debug_dir = tempfile::tempdir().expect("debug directory");
        let debug_path = debug_dir.path().join("debug.log");
        let provider = CustomProvider::from_config(&config)
            .expect("provider")
            .with_debug_log_path(&debug_path)
            .with_debug_requests(true);
        let stream = provider
            .chat(
                &[Message::user("hello")],
                &[],
                "",
                std::path::Path::new("."),
            )
            .await
            .expect("chat stream");

        let chunks = collect_chunks(stream).await;
        let request = request_rx.recv().expect("OpenAI chat request");
        assert_eq!(request.method, "POST");
        assert_eq!(request.url, "/v1/chat/completions");
        assert_eq!(request.content_type.as_deref(), Some("application/json"));
        assert_eq!(
            request.authorization.as_deref(),
            Some("Bearer custom-test-key")
        );
        assert_eq!(request.x_api_key, None);
        let payload: serde_json::Value = serde_json::from_str(&request.body).unwrap();
        assert_eq!(
            payload,
            json!({
                "model": "custom-openai-model",
                "messages": [{"role": "user", "content": "hello"}],
                "tools": null,
                "stream": true,
                "stream_options": {"include_usage": true},
                "max_tokens": 8192,
                "temperature": 0.699999988079071,
                "reasoning_effort": "low",
            })
        );
        assert!(matches!(&chunks[0], StreamChunk::TextDelta(text) if text == "Hello "));
        assert!(matches!(
            &chunks[1],
            StreamChunk::Usage {
                input_tokens: 5,
                output_tokens: 2
            }
        ));
        let debug_output = std::fs::read_to_string(debug_path).expect("debug log");
        assert!(debug_output.contains("custom OpenAI-compatible request"));
        assert!(debug_output.contains("method: POST"));
        assert!(debug_output.contains("/v1/chat/completions"));
        assert!(debug_output.contains("\"model\": \"custom-openai-model\""));
        assert!(debug_output.contains("authorization: [REDACTED]"));
        assert!(!debug_output.contains("data: "));
    }

    #[tokio::test]
    async fn custom_openai_responses_provider_streams_text_with_native_request_settings() {
        let body = concat!(
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hello\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":5,\"output_tokens\":2}}}\n\n"
        )
        .to_string();
        let (request_tx, request_rx) = mpsc::channel();
        let base_url = spawn_sse_server(body, 200, move |request| {
            request_tx
                .send(capture_chat_request(request))
                .expect("capture Responses request");
        });

        let mut config = NcaConfig::default();
        config.provider.custom.api_key = Some("custom-test-key".into());
        config.provider.custom.base_url = base_url;
        config.provider.custom.compatibility = ProviderCompatibility::OpenAiResponses;
        config.provider.custom.model = "responses-model".into();

        let debug_dir = tempfile::tempdir().expect("debug directory");
        let debug_path = debug_dir.path().join("debug.log");
        let provider = CustomProvider::from_config(&config)
            .expect("provider")
            .with_debug_log_path(&debug_path)
            .with_debug_requests(true);
        let stream = provider
            .chat(
                &[Message::system("be helpful"), Message::user("hello")],
                &[],
                "",
                Path::new("."),
            )
            .await
            .expect("chat stream");
        let chunks = collect_chunks(stream).await;
        let request = request_rx.recv().expect("Responses request");

        assert_eq!(request.method, "POST");
        assert_eq!(request.url, "/v1/responses");
        assert_eq!(
            request.authorization.as_deref(),
            Some("Bearer custom-test-key")
        );
        let payload: serde_json::Value = serde_json::from_str(&request.body).unwrap();
        assert_eq!(payload["model"], "responses-model");
        assert_eq!(payload["stream"], true);
        assert_eq!(payload["store"], false);
        assert_eq!(payload["input"][0]["role"], "system");
        assert_eq!(payload["input"][1]["role"], "user");
        assert_eq!(payload["input"][1]["content"], "hello");
        assert_eq!(payload.get("tools"), None);
        assert!(matches!(
            &chunks[0],
            StreamChunk::TextDelta(text) if text == "Hello"
        ));
        assert!(matches!(
            &chunks[1],
            StreamChunk::Usage {
                input_tokens: 5,
                output_tokens: 2
            }
        ));
        assert!(matches!(chunks.last(), Some(StreamChunk::Done)));
        let debug_output = std::fs::read_to_string(debug_path).expect("debug log");
        assert!(debug_output.contains("custom OpenAI Responses request"));
        assert!(debug_output.contains("method: POST"));
        assert!(debug_output.contains("/v1/responses"));
        assert!(debug_output.contains("\"model\": \"responses-model\""));
        assert!(debug_output.contains("authorization: [REDACTED]"));
        assert!(!debug_output.contains("event: "));
    }

    #[tokio::test]
    async fn custom_openai_responses_preserves_utf8_split_across_http_chunks() {
        let body = concat!(
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"世界\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{}}\n\n"
        )
        .as_bytes()
        .to_vec();
        let marker = b"\"delta\":\"";
        let delta_start = body
            .windows(marker.len())
            .position(|window| window == marker)
            .expect("find text delta")
            + marker.len();
        let split_at = delta_start + 1;
        let base_url =
            spawn_chunked_sse_server(vec![body[..split_at].to_vec(), body[split_at..].to_vec()]);

        let mut config = NcaConfig::default();
        config.provider.custom.api_key = Some("custom-test-key".into());
        config.provider.custom.base_url = base_url;
        config.provider.custom.compatibility = ProviderCompatibility::OpenAiResponses;

        let provider = CustomProvider::from_config(&config).expect("provider");
        let stream = provider
            .chat(&[Message::user("hello")], &[], "", Path::new("."))
            .await
            .expect("chat stream");
        let chunks = collect_chunks(stream).await;

        assert!(chunks.iter().any(|chunk| matches!(
            chunk,
            StreamChunk::TextDelta(text) if text == "世界"
        )));
        assert!(matches!(chunks.last(), Some(StreamChunk::Done)));
    }

    #[tokio::test]
    async fn custom_openai_responses_rejects_invalid_utf8_in_sse_lines() {
        let mut body = b"event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"".to_vec();
        body.push(0xff);
        body.extend_from_slice(b"\"}\n\n");
        let base_url = spawn_chunked_sse_server(vec![body]);

        let mut config = NcaConfig::default();
        config.provider.custom.api_key = Some("custom-test-key".into());
        config.provider.custom.base_url = base_url;
        config.provider.custom.compatibility = ProviderCompatibility::OpenAiResponses;

        let provider = CustomProvider::from_config(&config).expect("provider");
        let stream = provider
            .chat(&[Message::user("hello")], &[], "", Path::new("."))
            .await
            .expect("chat stream");
        let chunks = collect_chunks(stream).await;

        assert!(matches!(
            chunks.as_slice(),
            [StreamChunk::Error(message)]
                if message.contains("invalid UTF-8") && !message.contains('\u{fffd}')
        ));
    }

    #[tokio::test]
    async fn custom_openai_responses_provider_streams_native_function_calls_and_replays_outputs() {
        let body = concat!(
            "event: response.output_item.added\n",
            "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",\"call_id\":\"call_1\",\"name\":\"lookup\",\"arguments\":\"\"}}\n\n",
            "event: response.function_call_arguments.delta\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_1\",\"delta\":\"{\\\"path\\\":\\\"src\\\"}\"}\n\n",
            "event: response.function_call_arguments.done\n",
            "data: {\"type\":\"response.function_call_arguments.done\",\"item_id\":\"fc_1\",\"arguments\":\"{\\\"path\\\":\\\"src\\\"}\"}\n\n",
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",\"name\":\"lookup\",\"arguments\":\"{\\\"path\\\":\\\"src\\\"}\"}}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":12,\"output_tokens\":4}}}\n\n"
        )
        .to_string();
        let (request_tx, request_rx) = mpsc::channel();
        let base_url = spawn_sse_server(body, 200, move |request| {
            request_tx
                .send(capture_chat_request(request))
                .expect("capture Responses tool request");
        });

        let mut config = NcaConfig::default();
        config.provider.custom.api_key = Some("custom-test-key".into());
        config.provider.custom.base_url = base_url;
        config.provider.custom.compatibility = ProviderCompatibility::OpenAiResponses;
        config.provider.custom.model = "responses-model".into();

        let provider = CustomProvider::from_config(&config).expect("provider");
        let stream = provider
            .chat(
                &[
                    Message::user("look up src"),
                    Message::assistant_with_tool_calls(
                        "",
                        vec![nca_common::message::MessageToolCall {
                            id: "call_0".into(),
                            name: "lookup".into(),
                            arguments: json!({"path": "README.md"}),
                        }],
                    ),
                    Message::tool("call_0", "README contents"),
                ],
                &[ToolDefinition {
                    name: "lookup".into(),
                    description: "Look up a workspace path".into(),
                    parameters: json!({
                        "type": "object",
                        "properties": {"path": {"type": "string"}}
                    }),
                }],
                "",
                Path::new("."),
            )
            .await
            .expect("chat stream");
        let chunks = collect_chunks(stream).await;
        let request = request_rx.recv().expect("Responses tool request");
        let payload: serde_json::Value = serde_json::from_str(&request.body).unwrap();

        assert_eq!(
            payload["tools"],
            json!([{
                "type": "function",
                "name": "lookup",
                "description": "Look up a workspace path",
                "parameters": {
                    "type": "object",
                    "properties": {"path": {"type": "string"}}
                }
            }])
        );
        assert_eq!(payload["input"][1]["type"], "function_call");
        assert_eq!(payload["input"][1]["call_id"], "call_0");
        assert_eq!(payload["input"][1]["arguments"], r#"{"path":"README.md"}"#);
        assert_eq!(payload["input"][2]["type"], "function_call_output");
        assert_eq!(payload["input"][2]["call_id"], "call_0");
        assert_eq!(payload["input"][2]["output"], "README contents");
        assert!(chunks.iter().any(|chunk| matches!(
            chunk,
            StreamChunk::ToolUse(call)
                if call.id == "call_1"
                    && call.name == "lookup"
                    && call.input == json!({"path": "src"})
        )));
        assert!(chunks.iter().any(|chunk| matches!(
            chunk,
            StreamChunk::Usage {
                input_tokens: 12,
                output_tokens: 4
            }
        )));
        assert!(matches!(chunks.last(), Some(StreamChunk::Done)));
    }

    #[tokio::test]
    async fn custom_openai_responses_provider_uses_function_item_id_when_call_id_is_omitted() {
        let body = concat!(
            "event: response.output_item.added\n",
            "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\",\"id\":\"fc_gateway_1\",\"name\":\"lookup\",\"arguments\":\"\"}}\n\n",
            "event: response.function_call_arguments.delta\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_gateway_1\",\"delta\":\"{\\\"path\\\":\\\"src\\\"}\"}\n\n",
            "event: response.function_call_arguments.done\n",
            "data: {\"type\":\"response.function_call_arguments.done\",\"item_id\":\"fc_gateway_1\",\"arguments\":\"{\\\"path\\\":\\\"src\\\"}\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{}}\n\n"
        )
        .to_string();
        let base_url = spawn_sse_server(body, 200, |_| {});

        let mut config = NcaConfig::default();
        config.provider.custom.api_key = Some("custom-test-key".into());
        config.provider.custom.base_url = base_url;
        config.provider.custom.compatibility = ProviderCompatibility::OpenAiResponses;

        let provider = CustomProvider::from_config(&config).expect("provider");
        let stream = provider
            .chat(&[Message::user("look up src")], &[], "", Path::new("."))
            .await
            .expect("chat stream");
        let chunks = collect_chunks(stream).await;

        assert!(chunks.iter().any(|chunk| matches!(
            chunk,
            StreamChunk::ToolUse(call)
                if call.id == "fc_gateway_1"
                    && call.name == "lookup"
                    && call.input == json!({"path": "src"})
        )));
        assert!(matches!(chunks.last(), Some(StreamChunk::Done)));
    }

    #[tokio::test]
    async fn custom_openai_responses_provider_uses_output_index_when_item_id_changes() {
        let body = concat!(
            "event: response.output_item.added\n",
            "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"id\":\"stable-item\",\"call_id\":\"call_0\",\"name\":\"lookup\",\"arguments\":\"\"}}\n\n",
            "event: response.function_call_arguments.delta\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"rotating-item-1\",\"output_index\":0,\"delta\":\"{\\\"path\\\":\"}\n\n",
            "event: response.function_call_arguments.delta\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"rotating-item-2\",\"output_index\":0,\"delta\":\"\\\"src\\\"}\"}\n\n",
            "event: response.function_call_arguments.done\n",
            "data: {\"type\":\"response.function_call_arguments.done\",\"item_id\":\"rotating-item-done\",\"output_index\":0,\"arguments\":\"{\\\"path\\\":\\\"src\\\"}\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{}}\n\n"
        )
        .to_string();
        let base_url = spawn_sse_server(body, 200, |_| {});

        let mut config = NcaConfig::default();
        config.provider.custom.api_key = Some("custom-test-key".into());
        config.provider.custom.base_url = base_url;
        config.provider.custom.compatibility = ProviderCompatibility::OpenAiResponses;

        let provider = CustomProvider::from_config(&config).expect("provider");
        let stream = provider
            .chat(&[Message::user("look up src")], &[], "", Path::new("."))
            .await
            .expect("chat stream");
        let chunks = collect_chunks(stream).await;

        assert!(chunks.iter().any(|chunk| matches!(
            chunk,
            StreamChunk::ToolUse(call)
                if call.id == "call_0"
                    && call.name == "lookup"
                    && call.input == json!({"path": "src"})
        )));
        assert!(matches!(chunks.last(), Some(StreamChunk::Done)));
    }

    #[tokio::test]
    async fn custom_openai_responses_preserves_call_order_and_normalizes_multiple_identities() {
        let body = concat!(
            "event: response.output_item.added\n",
            "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"id\":\"item_0\",\"call_id\":\"call_0\",\"name\":\"first\",\"arguments\":\"\"}}\n\n",
            "event: response.function_call_arguments.delta\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"delta\":\"{\\\"value\\\":\\\"\"}\n\n",
            "event: response.function_call_arguments.delta\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"call_id\":\"call_0\",\"delta\":\"one\\\"}\"}\n\n",
            "event: response.function_call_arguments.done\n",
            "data: {\"type\":\"response.function_call_arguments.done\",\"call_id\":\"call_0\",\"arguments\":\"{\\\"value\\\":\\\"one\\\"}\"}\n\n",
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"id\":\"item_0\",\"name\":\"first\",\"arguments\":\"{\\\"value\\\":\\\"one\\\"}\"}}\n\n",
            "event: response.output_item.added\n",
            "data: {\"type\":\"response.output_item.added\",\"output_index\":1,\"item\":{\"type\":\"function_call\",\"name\":\"second\",\"arguments\":\"\"}}\n\n",
            "event: response.function_call_arguments.delta\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":1,\"delta\":\"{}\"}\n\n",
            "event: response.function_call_arguments.done\n",
            "data: {\"type\":\"response.function_call_arguments.done\",\"output_index\":1,\"arguments\":\"{}\"}\n\n",
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":1,\"item\":{\"type\":\"function_call\",\"name\":\"second\",\"arguments\":\"{}\"}}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{}}\n\n"
        )
        .to_string();
        let base_url = spawn_sse_server(body, 200, |_| {});
        let mut config = NcaConfig::default();
        config.provider.custom.api_key = Some("custom-test-key".into());
        config.provider.custom.base_url = base_url;
        config.provider.custom.compatibility = ProviderCompatibility::OpenAiResponses;

        let provider = CustomProvider::from_config(&config).expect("provider");
        let stream = provider
            .chat(&[Message::user("run both")], &[], "", Path::new("."))
            .await
            .expect("chat stream");
        let chunks = collect_chunks(stream).await;
        let calls = chunks
            .iter()
            .filter_map(|chunk| match chunk {
                StreamChunk::ToolUse(call) => Some((call.id.clone(), call.name.clone())),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(
            calls,
            [
                ("call_0".into(), "first".into()),
                ("1".into(), "second".into())
            ]
        );
        assert!(matches!(chunks.last(), Some(StreamChunk::Done)));
    }

    #[tokio::test]
    async fn custom_openai_responses_provider_maps_images_and_supported_generation_settings() {
        let body = concat!(
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n"
        )
        .to_string();
        let (request_tx, request_rx) = mpsc::channel();
        let base_url = spawn_sse_server(body, 200, move |request| {
            request_tx
                .send(capture_chat_request(request))
                .expect("capture Responses image request");
        });
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::write(workspace.path().join("sample.png"), [137, 80, 78, 71])
            .expect("write image fixture");

        let mut config = NcaConfig::default();
        config.provider.custom.api_key = Some("custom-test-key".into());
        config.provider.custom.base_url = base_url;
        config.provider.custom.compatibility = ProviderCompatibility::OpenAiResponses;
        config.provider.custom.model = "responses-model".into();
        config.model.max_tokens = 321;
        config.provider.custom.temperature = 0.25;
        config.model.reasoning_effort = "  low  ".into();

        let provider = CustomProvider::from_config(&config).expect("provider");
        let stream = provider
            .chat(
                &[Message::user_with_parts(vec![
                    ContentPart::Text {
                        text: "what is this?".into(),
                    },
                    ContentPart::Image {
                        media_type: "image/png".into(),
                        path: "sample.png".into(),
                    },
                ])],
                &[],
                "",
                workspace.path(),
            )
            .await
            .expect("chat stream");
        let chunks = collect_chunks(stream).await;
        let request = request_rx.recv().expect("Responses image request");
        let payload: serde_json::Value = serde_json::from_str(&request.body).unwrap();

        assert_eq!(payload["max_output_tokens"], 321);
        assert_eq!(payload["temperature"], 0.25);
        assert_eq!(payload["reasoning"], json!({"effort": "low"}));
        assert_eq!(
            payload["input"][0]["content"][0],
            json!({
                "type": "input_text",
                "text": "what is this?"
            })
        );
        assert_eq!(payload["input"][0]["content"][1]["type"], "input_image");
        assert_eq!(
            payload["input"][0]["content"][1]["image_url"],
            "data:image/png;base64,iVBORw=="
        );
        assert!(matches!(chunks.last(), Some(StreamChunk::Done)));
    }

    #[tokio::test]
    async fn custom_openai_responses_preserves_configured_temperature() {
        let server = Server::http("127.0.0.1:0").expect("start temperature compatibility server");
        let base_url = match server.server_addr() {
            tiny_http::ListenAddr::IP(addr) => format!("http://{addr}"),
            other => panic!("unsupported listen addr: {other:?}"),
        };
        let (request_tx, request_rx) = mpsc::channel();

        std::thread::spawn(move || {
            let mut request = server
                .recv()
                .expect("receive temperature compatibility request");
            let captured = capture_chat_request(&mut request);
            let payload: serde_json::Value = serde_json::from_str(&captured.body).unwrap();
            request_tx
                .send(captured)
                .expect("capture temperature compatibility request");

            let (status, body, content_type) = if payload["temperature"] == 0.25 {
                (
                    200,
                    concat!(
                        "event: response.output_text.delta\n",
                        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n\n",
                        "event: response.completed\n",
                        "data: {\"type\":\"response.completed\",\"response\":{}}\n\n"
                    ),
                    "text/event-stream",
                )
            } else {
                (
                    400,
                    r#"{"error":{"message":"Unsupported parameter: 'temperature' is not supported with this model.","code":"invalid_request_body"}}"#,
                    "application/json",
                )
            };
            request
                .respond(
                    Response::from_string(body)
                        .with_status_code(StatusCode(status))
                        .with_header(Header::from_bytes("Content-Type", content_type).unwrap()),
                )
                .expect("send temperature compatibility response");
        });

        let mut config = NcaConfig::default();
        config.provider.custom.api_key = Some("custom-test-key".into());
        config.provider.custom.base_url = base_url;
        config.provider.custom.compatibility = ProviderCompatibility::OpenAiResponses;
        config.provider.custom.temperature = 0.25;

        let provider = CustomProvider::from_config(&config).expect("provider");
        let stream = provider
            .chat(&[Message::user("hello")], &[], "", Path::new("."))
            .await
            .expect("Responses request should preserve configured temperature");
        let chunks = collect_chunks(stream).await;
        let request = request_rx
            .recv()
            .expect("temperature compatibility request");
        let payload: serde_json::Value = serde_json::from_str(&request.body).unwrap();

        assert_eq!(payload["temperature"], 0.25);
        assert!(matches!(chunks.last(), Some(StreamChunk::Done)));
    }

    #[tokio::test]
    async fn custom_openai_responses_rejects_unreadable_and_unsupported_images() {
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::write(workspace.path().join("notes.txt"), "not an image")
            .expect("write unsupported image fixture");
        let mut config = NcaConfig::default();
        config.provider.custom.api_key = Some("custom-test-key".into());
        config.provider.custom.base_url = "http://127.0.0.1:1".into();
        config.provider.custom.compatibility = ProviderCompatibility::OpenAiResponses;
        let provider = CustomProvider::from_config(&config).expect("provider");

        let unsupported = provider
            .chat(
                &[Message::user_with_parts(vec![ContentPart::Image {
                    media_type: "text/plain".into(),
                    path: "notes.txt".into(),
                }])],
                &[],
                "",
                workspace.path(),
            )
            .await;
        let unsupported = match unsupported {
            Ok(_) => panic!("unsupported image media type must fail before request"),
            Err(error) => error,
        };
        assert!(
            unsupported
                .to_string()
                .contains("unsupported image media type")
        );

        let unreadable = provider
            .chat(
                &[Message::user_with_parts(vec![ContentPart::Image {
                    media_type: "image/png".into(),
                    path: "missing.png".into(),
                }])],
                &[],
                "",
                workspace.path(),
            )
            .await;
        let unreadable = match unreadable {
            Ok(_) => panic!("unreadable image must fail before request"),
            Err(error) => error,
        };
        assert!(unreadable.to_string().contains("failed to read image"));
    }

    #[tokio::test]
    async fn custom_openai_responses_rejects_function_calls_without_identity() {
        let body = concat!(
            "event: response.output_item.added\n",
            "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\",\"name\":\"lookup\",\"arguments\":\"\"}}\n\n",
            "event: response.function_call_arguments.delta\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"delta\":\"{}\"}\n\n"
        )
        .to_string();
        let base_url = spawn_sse_server(body, 200, |_| {});
        let mut config = NcaConfig::default();
        config.provider.custom.api_key = Some("custom-test-key".into());
        config.provider.custom.base_url = base_url;
        config.provider.custom.compatibility = ProviderCompatibility::OpenAiResponses;

        let provider = CustomProvider::from_config(&config).expect("provider");
        let stream = provider
            .chat(&[Message::user("look up src")], &[], "", Path::new("."))
            .await
            .expect("chat stream");
        let chunks = collect_chunks(stream).await;

        assert!(matches!(
            chunks.as_slice(),
            [StreamChunk::Error(message)] if message.contains("stable identity")
        ));
    }

    #[tokio::test]
    async fn custom_openai_responses_rejects_invalid_arguments_and_incomplete_calls() {
        let invalid_body = concat!(
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"id\":\"fc_invalid\",\"name\":\"lookup\",\"arguments\":\"not-json\"}}\n\n"
        )
        .to_string();
        let base_url = spawn_sse_server(invalid_body, 200, |_| {});
        let mut config = NcaConfig::default();
        config.provider.custom.api_key = Some("custom-test-key".into());
        config.provider.custom.base_url = base_url;
        config.provider.custom.compatibility = ProviderCompatibility::OpenAiResponses;

        let provider = CustomProvider::from_config(&config).expect("provider");
        let stream = provider
            .chat(&[Message::user("invalid args")], &[], "", Path::new("."))
            .await
            .expect("chat stream");
        let chunks = collect_chunks(stream).await;
        assert!(matches!(
            chunks.as_slice(),
            [StreamChunk::Error(message)] if message.contains("invalid JSON")
        ));

        let incomplete_body = concat!(
            "event: response.output_item.added\n",
            "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\",\"id\":\"fc_incomplete\",\"name\":\"lookup\",\"arguments\":\"\"}}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{}}\n\n"
        )
        .to_string();
        config.provider.custom.base_url = spawn_sse_server(incomplete_body, 200, |_| {});
        let provider = CustomProvider::from_config(&config).expect("provider");
        let stream = provider
            .chat(&[Message::user("incomplete args")], &[], "", Path::new("."))
            .await
            .expect("chat stream");
        let chunks = collect_chunks(stream).await;
        assert!(matches!(
            chunks.as_slice(),
            [StreamChunk::Error(message)] if message.contains("incomplete")
        ));
    }

    #[tokio::test]
    async fn custom_openai_responses_provider_ignores_unknown_events_but_rejects_empty_completion()
    {
        let body = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\"}\n\n",
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{}}\n\n"
        )
        .to_string();
        let base_url = spawn_sse_server(body, 200, |_| {});
        let mut config = NcaConfig::default();
        config.provider.custom.api_key = Some("custom-test-key".into());
        config.provider.custom.base_url = base_url;
        config.provider.custom.compatibility = ProviderCompatibility::OpenAiResponses;

        let provider = CustomProvider::from_config(&config).expect("provider");
        let stream = provider
            .chat(&[Message::user("hello")], &[], "", Path::new("."))
            .await
            .expect("chat stream");
        let chunks = collect_chunks(stream).await;
        assert!(matches!(chunks.first(), Some(StreamChunk::TextDelta(text)) if text == "ok"));
        assert!(matches!(chunks.last(), Some(StreamChunk::Done)));

        let empty_body = concat!(
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{}}\n\n"
        )
        .to_string();
        let empty_base_url = spawn_sse_server(empty_body, 200, |_| {});
        config.provider.custom.base_url = empty_base_url;
        let provider = CustomProvider::from_config(&config).expect("provider");
        let stream = provider
            .chat(&[Message::user("hello")], &[], "", Path::new("."))
            .await
            .expect("chat stream");
        let chunks = collect_chunks(stream).await;
        assert!(matches!(
            chunks.as_slice(),
            [StreamChunk::Error(message)] if message.contains("empty")
        ));
    }

    #[tokio::test]
    async fn custom_openai_responses_provider_rejects_malformed_known_events() {
        let body = concat!(
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\"}\n\n"
        )
        .to_string();
        let base_url = spawn_sse_server(body, 200, |_| {});
        let mut config = NcaConfig::default();
        config.provider.custom.api_key = Some("custom-test-key".into());
        config.provider.custom.base_url = base_url;
        config.provider.custom.compatibility = ProviderCompatibility::OpenAiResponses;

        let provider = CustomProvider::from_config(&config).expect("provider");
        let stream = provider
            .chat(&[Message::user("hello")], &[], "", Path::new("."))
            .await
            .expect("chat stream");
        let chunks = collect_chunks(stream).await;
        assert!(matches!(
            chunks.as_slice(),
            [StreamChunk::Error(message)] if message.contains("missing a string delta")
        ));
    }

    #[tokio::test]
    async fn custom_openai_provider_preserves_versioned_path_prefix() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"},\"index\":0,\"finish_reason\":null}]}\n\n",
            "data: [DONE]\n\n"
        )
        .to_string();
        let (request_tx, request_rx) = mpsc::channel();
        let origin = spawn_sse_server(body, 200, move |request| {
            request_tx
                .send(capture_chat_request(request))
                .expect("capture OpenAI chat request");
        });

        let mut config = NcaConfig::default();
        config.provider.custom.api_key = Some("custom-test-key".into());
        config.provider.custom.base_url = format!("{origin}/zen/v1");
        config.provider.custom.compatibility = ProviderCompatibility::OpenAi;

        let provider = CustomProvider::from_config(&config).expect("provider");
        let stream = provider
            .chat(&[Message::user("hello")], &[], "", Path::new("."))
            .await
            .expect("chat stream");
        let chunks = collect_chunks(stream).await;

        let request = request_rx.recv().expect("OpenAI chat request");
        assert_eq!(request.url, "/zen/v1/chat/completions");
        assert!(matches!(chunks.last(), Some(StreamChunk::Done)));
    }

    #[tokio::test]
    async fn custom_openai_compatible_provider_omits_default_reasoning_effort() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"},\"index\":0,\"finish_reason\":null}]}\n\n",
            "data: [DONE]\n\n"
        )
        .to_string();
        let (request_tx, request_rx) = mpsc::channel();
        let base_url = spawn_sse_server(body, 200, move |request| {
            request_tx
                .send(capture_chat_request(request))
                .expect("capture OpenAI chat request");
        });

        let mut config = NcaConfig::default();
        config.provider.custom.api_key = Some("custom-test-key".into());
        config.provider.custom.base_url = base_url;
        config.provider.custom.compatibility = ProviderCompatibility::OpenAi;

        let provider = CustomProvider::from_config(&config).expect("provider");
        let stream = provider
            .chat(&[Message::user("hello")], &[], "", Path::new("."))
            .await
            .expect("chat stream");
        let chunks = collect_chunks(stream).await;
        let request = request_rx.recv().expect("OpenAI chat request");
        let payload: serde_json::Value = serde_json::from_str(&request.body).unwrap();

        assert!(payload.get("reasoning_effort").is_none());
        assert!(matches!(chunks.last(), Some(StreamChunk::Done)));
    }

    #[tokio::test]
    async fn custom_anthropic_compatible_provider_streams_tools() {
        let body = concat!(
            "event: content_block_start\n",
            "data: {\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"lookup\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"src\\\"}\"}}\n\n",
            "event: content_block_stop\n",
            "data: {}\n\n",
            "event: message_delta\n",
            "data: {\"usage\":{\"output_tokens\":5}}\n\n"
        )
        .to_string();
        let (request_tx, request_rx) = mpsc::channel();
        let base_url = spawn_sse_server(body, 200, move |request| {
            request_tx
                .send(capture_chat_request(request))
                .expect("capture Anthropic chat request");
        });

        let mut config = NcaConfig::default();
        config.provider.custom.api_key = Some("custom-test-key".into());
        config.provider.custom.base_url = base_url;
        config.provider.custom.compatibility = ProviderCompatibility::Anthropic;
        config.provider.custom.model = "custom-anthropic-model".into();
        config.model.reasoning_effort = "high".into();

        let debug_dir = tempfile::tempdir().expect("debug directory");
        let debug_path = debug_dir.path().join("debug.log");
        let provider = CustomProvider::from_config(&config)
            .expect("provider")
            .with_debug_log_path(&debug_path)
            .with_debug_requests(true);
        let stream = provider
            .chat(
                &[Message::user("hello")],
                &[ToolDefinition {
                    name: "lookup".into(),
                    description: "Lookup a path".into(),
                    parameters: json!({
                        "type": "object",
                        "properties": {"path": {"type": "string"}}
                    }),
                }],
                "",
                std::path::Path::new("."),
            )
            .await
            .expect("chat stream");

        let chunks = collect_chunks(stream).await;
        let request = request_rx.recv().expect("Anthropic chat request");
        assert_eq!(request.method, "POST");
        assert_eq!(request.url, "/v1/messages");
        assert_eq!(request.content_type.as_deref(), Some("application/json"));
        assert_eq!(request.x_api_key.as_deref(), Some("custom-test-key"));
        assert_eq!(request.anthropic_version.as_deref(), Some("2023-06-01"));
        assert_eq!(request.authorization, None);
        let payload: serde_json::Value = serde_json::from_str(&request.body).unwrap();
        assert_eq!(
            payload,
            json!({
                "model": "custom-anthropic-model",
                "max_tokens": 8192,
                "system": null,
                "messages": [{"role": "user", "content": "hello"}],
                "tools": [{
                    "name": "lookup",
                    "description": "Lookup a path",
                    "input_schema": {
                        "type": "object",
                        "properties": {"path": {"type": "string"}}
                    }
                }],
                "stream": true,
                "temperature": 0.699999988079071,
            })
        );
        assert!(matches!(
            &chunks[0],
            StreamChunk::ToolUse(call) if call.name == "lookup" && call.input == json!({"path":"src"})
        ));
        assert!(!debug_path.exists());
    }

    #[tokio::test]
    async fn custom_openai_compatible_provider_streams_tools() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"lookup\",\"arguments\":\"{\\\"path\\\":\\\"src\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n"
        )
        .to_string();
        let (request_tx, request_rx) = mpsc::channel();
        let base_url = spawn_sse_server(body, 200, move |request| {
            request_tx
                .send(capture_chat_request(request))
                .expect("capture OpenAI tool chat request");
        });

        let mut config = NcaConfig::default();
        config.provider.custom.api_key = Some("custom-test-key".into());
        config.provider.custom.base_url = base_url;
        config.provider.custom.compatibility = ProviderCompatibility::OpenAi;
        config.provider.custom.model = "custom-openai-model".into();

        let provider = CustomProvider::from_config(&config).expect("provider");
        let stream = provider
            .chat(
                &[Message::user("look up src")],
                &[ToolDefinition {
                    name: "lookup".into(),
                    description: "Lookup a path".into(),
                    parameters: json!({
                        "type": "object",
                        "properties": {"path": {"type": "string"}}
                    }),
                }],
                "",
                std::path::Path::new("."),
            )
            .await
            .expect("chat stream");

        let chunks = collect_chunks(stream).await;
        let request = request_rx.recv().expect("OpenAI tool chat request");
        assert_eq!(request.method, "POST");
        assert_eq!(request.url, "/v1/chat/completions");
        assert_eq!(request.content_type.as_deref(), Some("application/json"));
        assert_eq!(
            request.authorization.as_deref(),
            Some("Bearer custom-test-key")
        );
        assert_eq!(request.x_api_key, None);
        assert_eq!(request.anthropic_version, None);
        let payload: serde_json::Value = serde_json::from_str(&request.body).unwrap();
        assert_eq!(
            payload,
            json!({
                "model": "custom-openai-model",
                "messages": [{"role": "user", "content": "look up src"}],
                "tools": [{
                    "type": "function",
                    "function": {
                        "name": "lookup",
                        "description": "Lookup a path",
                        "parameters": {
                            "type": "object",
                            "properties": {"path": {"type": "string"}}
                        }
                    }
                }],
                "stream": true,
                "stream_options": {"include_usage": true},
                "max_tokens": 8192,
                "temperature": 0.699999988079071,
            })
        );
        assert!(chunks.iter().any(|chunk| matches!(
            chunk,
            StreamChunk::ToolUse(call)
                if call.id == "call_1"
                    && call.name == "lookup"
                    && call.input == json!({"path": "src"})
        )));
        assert!(
            chunks
                .iter()
                .any(|chunk| matches!(chunk, StreamChunk::Done))
        );
    }

    #[test]
    fn request_debugging_requires_exact_one_value() {
        assert!(!debug_request_enabled(None));
        assert!(!debug_request_enabled(Some("")));
        assert!(!debug_request_enabled(Some("0")));
        assert!(!debug_request_enabled(Some("true")));
        assert!(!debug_request_enabled(Some(" 1")));
        assert!(debug_request_enabled(Some("1")));
    }

    #[test]
    fn request_debug_output_includes_request_and_redacts_credentials() {
        let request = reqwest::Client::new()
            .post("https://gateway.example/v1/chat/completions?token=request-secret")
            .header("authorization", "Bearer request-secret")
            .header("x-api-key", "request-secret")
            .json(&json!({"message": "request-secret"}))
            .build()
            .expect("request");

        let output = format_debug_request(&request, "request-secret", "OpenAI-compatible");

        assert!(output.starts_with('['));
        assert!(output.contains("] custom OpenAI-compatible request\n"));
        assert!(output.contains("method: POST"));
        assert!(output.contains("url: https://gateway.example/v1/chat/completions"));
        assert!(output.contains("token=[REDACTED]"));
        assert!(output.contains("  authorization: [REDACTED]"));
        assert!(output.contains("  x-api-key: [REDACTED]"));
        assert!(output.contains("body:\n{\n  \"message\": \"[REDACTED]\"\n}"));
        assert!(!output.contains("request-secret"));
    }

    #[test]
    fn request_debug_log_appends_records_without_logging_responses() {
        let temp_dir = tempfile::tempdir().expect("debug directory");
        let path = temp_dir.path().join("debug.log");
        let request = reqwest::Client::new()
            .post("https://gateway.example/v1/responses")
            .json(&json!({"model": "test-model", "input": "hello"}))
            .build()
            .expect("request");

        append_debug_log(
            &path,
            &format_debug_request(&request, "unused-secret", "OpenAI Responses"),
        )
        .expect("first log record");
        append_debug_log(
            &path,
            &format_debug_request(&request, "unused-secret", "OpenAI Responses"),
        )
        .expect("second log record");

        let output = std::fs::read_to_string(path).expect("debug log");
        assert_eq!(output.matches("custom OpenAI Responses request").count(), 2);
        assert!(
            output.contains("body:\n{\n  \"input\": \"hello\",\n  \"model\": \"test-model\"\n}")
        );
        assert!(!output.contains("response.completed"));
    }

    #[test]
    fn request_debug_log_failure_is_reported_without_writing_a_file() {
        let temp_dir = tempfile::tempdir().expect("debug directory");
        let path = temp_dir.path().join("missing").join("debug.log");
        let error = append_debug_log(&path, "request diagnostic\n")
            .expect_err("missing parent directory prevents log creation");
        assert!(error.kind() == std::io::ErrorKind::NotFound);
        assert!(!path.exists());
    }
}
