use std::path::Path;

use nca_common::config::{
    CustomProviderConfig, CustomProviderConfigError, NcaConfig, ProviderCompatibility,
    custom_provider_endpoint, normalize_custom_provider_base_url, validate_custom_api_key_env_name,
};
use nca_common::message::Message;
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
            ProviderCompatibility::OpenAi => Self::OpenAi,
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

fn custom_adapter(compatibility: ProviderCompatibility) -> &'static dyn CustomProtocolAdapter {
    match compatibility {
        ProviderCompatibility::OpenAi => &OPENAI_CUSTOM_ADAPTER,
        ProviderCompatibility::Anthropic => &ANTHROPIC_CUSTOM_ADAPTER,
    }
}

pub struct CustomProvider {
    client: reqwest::Client,
    config: CustomProviderConfig,
    max_tokens: u32,
    reasoning_effort: String,
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
        })
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
    let body = body.replace(api_key, "[REDACTED]");
    let body = body.trim();
    if body.is_empty() {
        return "custom provider returned an empty error response".into();
    }
    body.chars().take(512).collect()
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
        let response = adapter
            .chat_request(&self.client, &self.config.base_url, body, &api_key)?
            .send()
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

        let provider = CustomProvider::from_config(&config).expect("provider");
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

        let provider = CustomProvider::from_config(&config).expect("provider");
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
}
