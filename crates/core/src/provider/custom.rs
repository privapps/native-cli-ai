use std::path::Path;

use nca_common::config::{
    CustomProviderConfig, CustomProviderConfigError, NcaConfig, ProviderCompatibility,
    normalize_custom_provider_base_url, validate_custom_api_key_env_name,
};
use nca_common::message::Message;
use nca_common::tool::ToolDefinition;
use reqwest::header::HeaderValue;
use std::time::Duration;

use super::anthropic_compat::{anthropic_request_body, spawn_anthropic_stream};
use super::openai_compat::{openai_request_body, spawn_openai_stream};
use super::{Provider, ProviderError, StreamChunk};

pub struct CustomProvider {
    client: reqwest::Client,
    config: CustomProviderConfig,
    max_tokens: u32,
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

        match custom.compatibility {
            ProviderCompatibility::OpenAi => {
                HeaderValue::from_str(&format!("Bearer {api_key}")).map_err(|err| {
                    ProviderError::Configuration(format!(
                        "failed to build Custom provider authorization header: {err}"
                    ))
                })?;
            }
            ProviderCompatibility::Anthropic => {
                HeaderValue::from_str(&api_key).map_err(|err| {
                    ProviderError::Configuration(format!(
                        "failed to build Custom provider x-api-key header: {err}"
                    ))
                })?;
            }
        }

        let client = reqwest::Client::builder().build().map_err(|err| {
            ProviderError::Configuration(format!("failed to build HTTP client: {err}"))
        })?;

        Ok(Self {
            client,
            config: custom,
            max_tokens: config.model.max_tokens,
        })
    }

    fn endpoint(&self) -> String {
        match self.config.compatibility {
            ProviderCompatibility::OpenAi => format!(
                "{}/v1/chat/completions",
                self.config.base_url.trim_end_matches('/')
            ),
            ProviderCompatibility::Anthropic => {
                format!("{}/v1/messages", self.config.base_url.trim_end_matches('/'))
            }
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
    let response = match config.compatibility {
        ProviderCompatibility::OpenAi => {
            client
                .get(format!("{base_url}/v1/models"))
                .bearer_auth(&api_key)
                .send()
                .await
        }
        ProviderCompatibility::Anthropic => {
            client
                .post(format!("{base_url}/v1/messages"))
                .header("x-api-key", &api_key)
                .header("anthropic-version", "2023-06-01")
                .json(&serde_json::json!({
                    "model": config.model,
                    "max_tokens": 1,
                    "messages": [{"role": "user", "content": "ping"}],
                }))
                .send()
                .await
        }
    }
    .map_err(|error| {
        ProviderError::RequestFailed(sanitize_provider_error(&error.to_string(), &api_key))
    })?;

    if response.status().is_success() {
        return Ok(());
    }

    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    Err(map_custom_provider_error(status, body, &api_key))
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

        match self.config.compatibility {
            ProviderCompatibility::OpenAi => {
                let body = openai_request_body(
                    messages,
                    tools,
                    &model,
                    self.max_tokens,
                    self.config.temperature,
                    workspace_root,
                )?;

                let response = self
                    .client
                    .post(self.endpoint())
                    .bearer_auth(&api_key)
                    .json(&body)
                    .send()
                    .await
                    .map_err(|err| ProviderError::RequestFailed(err.to_string()))?;

                let status = response.status();
                if !status.is_success() {
                    let body_text = response.text().await.unwrap_or_default();
                    return Err(map_custom_provider_error(status, body_text, &api_key));
                }

                Ok(spawn_openai_stream(response, "custom"))
            }
            ProviderCompatibility::Anthropic => {
                let body = anthropic_request_body(
                    messages,
                    tools,
                    &model,
                    self.max_tokens,
                    self.config.temperature,
                    workspace_root,
                )?;

                let response = self
                    .client
                    .post(self.endpoint())
                    .header("x-api-key", &api_key)
                    .header("anthropic-version", "2023-06-01")
                    .json(&body)
                    .send()
                    .await
                    .map_err(|err| ProviderError::RequestFailed(err.to_string()))?;

                let status = response.status();
                if !status.is_success() {
                    let body_text = response.text().await.unwrap_or_default();
                    return Err(map_custom_provider_error(status, body_text, &api_key));
                }

                Ok(spawn_anthropic_stream(response, "custom"))
            }
        }
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
    async fn anthropic_probe_sends_minimal_messages_request_with_required_headers() {
        let (base_url, request_rx) = spawn_probe_server(200, r#"{"id":"msg_1"}"#);
        let config = custom_config(ProviderCompatibility::Anthropic, &base_url, "probe-secret");

        probe_custom_provider(&config)
            .await
            .expect("probe succeeds");

        let request = request_rx.recv().expect("probe request");
        assert_eq!(request.method, "POST");
        assert_eq!(request.url, "/v1/messages");
        assert_eq!(request.x_api_key.as_deref(), Some("probe-secret"));
        assert_eq!(request.anthropic_version.as_deref(), Some("2023-06-01"));
        assert!(request.body.contains("\"messages\""));
        assert!(request.body.contains("\"max_tokens\""));
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
