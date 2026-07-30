//! Optional provider HTTP lookups for context window sizes.
//!
//! - [OpenRouter](https://openrouter.ai/docs/api-reference/models/get-models): public `GET .../models`
//!   with `context_length` per model.
//! - Anthropic: `GET /v1/models` (requires API key); entries may include `max_input_tokens`.
//! - OpenAI: `GET /v1/models` (requires key); `context_window` is present on some responses.
//! - **MiniMax**: the Anthropic-compatible host (`api.minimax.io/anthropic`) does not expose
//!   `/v1/models` (404); context limits stay on the built-in [`crate::model_limits`] table.
//!
//! Successful catalog responses are cached in memory per process. Tune with
//! `NCA_CONTEXT_API_CACHE_TTL_SECS` (default `3600`). Use `NCA_SKIP_CONTEXT_API=1` to disable
//! lookups entirely.

use crate::model_limits::ModelLimits;
use nca_common::config::{
    NcaConfig, ProviderCapabilitySupport, ProviderCompatibility, ProviderKind,
    ResolvedProviderSettings, custom_provider_endpoint,
};
use nca_core::provider::custom::{CustomCapabilityAdapter, CustomModelCatalogEntry};
use serde::Deserialize;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const HTTP_TIMEOUT_SECS: u64 = 12;

const PROVIDER_CAPABILITY_CHECKLIST: [ProviderCapabilitySupport; ProviderKind::ALL.len()] = [
    ProviderKind::MiniMax.capability_support(),
    ProviderKind::OpenAi.capability_support(),
    ProviderKind::Anthropic.capability_support(),
    ProviderKind::OpenRouter.capability_support(),
    ProviderKind::Custom.capability_support(),
];

const _: () = {
    let mut index = 0;
    while index < PROVIDER_CAPABILITY_CHECKLIST.len() {
        let support = PROVIDER_CAPABILITY_CHECKLIST[index];
        assert!(support.provider.is(ProviderKind::ALL[index]));
        assert!(support.settings);
        assert!(support.model_catalog);
        assert!(support.context_window);
        index += 1;
    }
};

fn catalog_cache_ttl() -> Duration {
    std::env::var("NCA_CONTEXT_API_CACHE_TTL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&n| n > 0)
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(3600))
}

fn api_key_tag(secret: &str) -> u64 {
    let mut h = DefaultHasher::new();
    secret.hash(&mut h);
    h.finish()
}

fn cache_stale(fetched_at: Instant, ttl: Duration) -> bool {
    fetched_at.elapsed() >= ttl
}

/// Cache identity deliberately contains provider and protocol identity in
/// addition to endpoint and credential identity. The credential itself never
/// enters a cache key, log field, or diagnostic.
#[derive(Clone, Debug, PartialEq, Eq)]
struct CapabilityCacheKey {
    provider: ProviderKind,
    endpoint: String,
    compatibility: Option<ProviderCompatibility>,
    model: Option<String>,
    credential_tag: Option<u64>,
}

fn catalog_endpoint(settings: &ResolvedProviderSettings) -> Option<String> {
    settings
        .normalized_base_url()
        .map(|base| custom_provider_endpoint(&base, "models"))
}

fn cache_key(
    settings: &ResolvedProviderSettings,
    model: Option<&str>,
) -> Option<CapabilityCacheKey> {
    Some(CapabilityCacheKey {
        provider: settings.provider,
        endpoint: catalog_endpoint(settings)?,
        compatibility: settings.compatibility,
        model: model.map(str::to_owned),
        credential_tag: settings.credential().map(api_key_tag),
    })
}

enum ProviderCapabilityAdapter {
    MiniMax {
        settings: ResolvedProviderSettings,
    },
    OpenRouter {
        settings: ResolvedProviderSettings,
    },
    OpenAi {
        settings: ResolvedProviderSettings,
    },
    Anthropic {
        settings: ResolvedProviderSettings,
    },
    Custom {
        settings: ResolvedProviderSettings,
        protocol: CustomCapabilityAdapter,
    },
}

impl ProviderCapabilityAdapter {
    /// The only provider/compatibility dispatch point for model capabilities.
    fn from_config(config: &NcaConfig) -> Self {
        let settings = config.provider.active_settings();
        match settings.provider {
            ProviderKind::MiniMax => Self::MiniMax { settings },
            ProviderKind::OpenRouter => Self::OpenRouter { settings },
            ProviderKind::OpenAi => Self::OpenAi { settings },
            ProviderKind::Anthropic => Self::Anthropic { settings },
            ProviderKind::Custom => Self::Custom {
                protocol: CustomCapabilityAdapter::from_compatibility(
                    settings
                        .compatibility
                        .expect("custom provider compatibility is always configured"),
                ),
                settings,
            },
        }
    }

    fn settings(&self) -> &ResolvedProviderSettings {
        match self {
            Self::MiniMax { settings }
            | Self::OpenRouter { settings }
            | Self::OpenAi { settings }
            | Self::Anthropic { settings }
            | Self::Custom { settings, .. } => settings,
        }
    }

    async fn context_window(&self, client: &reqwest::Client, model: &str) -> Option<usize> {
        let settings = self.settings();
        if matches!(self, Self::MiniMax { .. }) {
            return None;
        }
        if !settings.can_query_catalog() {
            tracing::debug!(
                provider = %settings.provider.display_name(),
                credential_env = %settings.api_key_env,
                configured_model = %settings.model,
                "context API: provider credential is not configured"
            );
            return None;
        }

        let key = cache_key(settings, Some(model))?;
        let ttl = catalog_cache_ttl();
        if let Some(context_window) = cached_context_window(&key, ttl) {
            return context_window;
        }

        let context_window = match self {
            Self::MiniMax { .. } => None,
            Self::OpenRouter { settings } => {
                let catalog = fetch_openrouter_catalog(client, settings).await?;
                pick_openrouter(catalog.as_ref(), model)
                    .and_then(|model| model.context_length)
                    .map(|value| value as usize)
            }
            Self::OpenAi { settings } => {
                let catalog = fetch_openai_catalog(client, settings).await?;
                openai_context_from_catalog(catalog.as_ref(), model)
            }
            Self::Anthropic { settings } => {
                let catalog = fetch_anthropic_catalog(client, settings).await?;
                pick_anthropic(catalog.as_ref(), model)
                    .and_then(|model| model.max_input_tokens)
                    .map(|value| value as usize)
            }
            Self::Custom { settings, protocol } => {
                let catalog = fetch_custom_catalog(client, settings, *protocol).await?;
                pick_custom(catalog.as_ref(), model, *protocol)
                    .and_then(|model| model.context_window)
                    .map(|value| value as usize)
            }
        };
        store_context_window(key, context_window);
        context_window
    }

    async fn model_ids(&self, client: &reqwest::Client) -> Vec<String> {
        match self {
            Self::MiniMax { .. } => vec!["MiniMax-M2.5".into(), "MiniMax-M2.7".into()],
            Self::OpenRouter { settings } => fetch_openrouter_catalog(client, settings)
                .await
                .map(|models| sorted_ids(models.iter().map(|model| model.id.clone())))
                .unwrap_or_default(),
            Self::OpenAi { settings } => fetch_openai_catalog(client, settings)
                .await
                .map(|catalog| {
                    catalog
                        .get("data")
                        .and_then(|data| data.as_array())
                        .map(|models| {
                            sorted_ids(models.iter().filter_map(|model| {
                                model
                                    .get("id")
                                    .and_then(|value| value.as_str())
                                    .map(str::to_owned)
                            }))
                        })
                        .unwrap_or_default()
                })
                .unwrap_or_default(),
            Self::Anthropic { settings } => fetch_anthropic_catalog(client, settings)
                .await
                .map(|models| sorted_ids(models.iter().map(|model| model.id.clone())))
                .unwrap_or_default(),
            Self::Custom { settings, protocol } => {
                fetch_custom_catalog(client, settings, *protocol)
                    .await
                    .map(|models| sorted_ids(models.iter().map(|model| model.id.clone())))
                    .unwrap_or_default()
            }
        }
    }
}

// --- Catalog caches ---

struct OpenRouterCatalogEntry {
    key: CapabilityCacheKey,
    fetched_at: Instant,
    models: Arc<Vec<OpenRouterModel>>,
}

fn openrouter_catalog_cache() -> &'static Mutex<Option<OpenRouterCatalogEntry>> {
    static CELL: OnceLock<Mutex<Option<OpenRouterCatalogEntry>>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(None))
}

// --- Anthropic ---

struct AnthropicCatalogEntry {
    key: CapabilityCacheKey,
    fetched_at: Instant,
    models: Arc<Vec<AnthropicModel>>,
}

fn anthropic_catalog_cache() -> &'static Mutex<Option<AnthropicCatalogEntry>> {
    static CELL: OnceLock<Mutex<Option<AnthropicCatalogEntry>>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(None))
}

// --- OpenAI ---

struct OpenAiCatalogEntry {
    key: CapabilityCacheKey,
    fetched_at: Instant,
    value: Arc<serde_json::Value>,
}

fn openai_catalog_cache() -> &'static Mutex<Option<OpenAiCatalogEntry>> {
    static CELL: OnceLock<Mutex<Option<OpenAiCatalogEntry>>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(None))
}

// --- Custom protocol catalogs ---

struct CustomCatalogEntry {
    key: CapabilityCacheKey,
    fetched_at: Instant,
    models: Arc<Vec<CustomModelCatalogEntry>>,
}

fn custom_catalog_cache() -> &'static Mutex<Option<CustomCatalogEntry>> {
    static CELL: OnceLock<Mutex<Option<CustomCatalogEntry>>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(None))
}

struct ContextCacheEntry {
    key: CapabilityCacheKey,
    fetched_at: Instant,
    context_window: Option<usize>,
}

fn context_cache() -> &'static Mutex<Vec<ContextCacheEntry>> {
    static CELL: OnceLock<Mutex<Vec<ContextCacheEntry>>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(Vec::new()))
}

fn cached_context_window(key: &CapabilityCacheKey, ttl: Duration) -> Option<Option<usize>> {
    let guard = context_cache().lock().ok()?;
    guard
        .iter()
        .find(|entry| entry.key == *key && !cache_stale(entry.fetched_at, ttl))
        .map(|entry| entry.context_window)
}

fn store_context_window(key: CapabilityCacheKey, context_window: Option<usize>) {
    if let Ok(mut guard) = context_cache().lock() {
        guard.retain(|entry| entry.key != key);
        guard.push(ContextCacheEntry {
            key,
            fetched_at: Instant::now(),
            context_window,
        });
    }
}

fn sorted_ids(ids: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut ids: Vec<String> = ids.into_iter().collect();
    ids.sort();
    ids
}

pub async fn resolve_model_limits(config: &NcaConfig, model: &str) -> ModelLimits {
    let static_limits = ModelLimits::for_model(model);

    if std::env::var("NCA_SKIP_CONTEXT_API").ok().as_deref() == Some("1") {
        return static_limits;
    }

    if !config.memory.context.auto_detect_context_window
        || !config.memory.context.query_provider_models_api
    {
        return static_limits;
    }

    let client = match http_client() {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!(error = %e, "context API: failed to build HTTP client");
            return static_limits;
        }
    };

    let capability = ProviderCapabilityAdapter::from_config(config);
    let from_api = capability.context_window(&client, model).await;

    match from_api {
        Some(cw) if cw > 0 => {
            tracing::info!(
                model = %model,
                context_window = cw,
                "context window from provider models API"
            );
            ModelLimits {
                context_window: cw,
                max_output_tokens: static_limits.max_output_tokens,
            }
        }
        _ => static_limits,
    }
}

fn http_client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(HTTP_TIMEOUT_SECS))
        .user_agent(concat!(
            "nca/",
            env!("CARGO_PKG_VERSION"),
            " (context-window lookup)"
        ))
        .build()
}

#[derive(Debug, Deserialize)]
struct OpenRouterModelsResponse {
    data: Vec<OpenRouterModel>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterModel {
    id: String,
    context_length: Option<u64>,
}

async fn fetch_openrouter_catalog(
    client: &reqwest::Client,
    settings: &ResolvedProviderSettings,
) -> Option<Arc<Vec<OpenRouterModel>>> {
    let key = cache_key(settings, None)?;
    let ttl = catalog_cache_ttl();
    {
        let guard = openrouter_catalog_cache().lock().ok()?;
        if let Some(entry) = guard.as_ref()
            && entry.key == key
            && !cache_stale(entry.fetched_at, ttl)
        {
            tracing::debug!("openrouter models catalog cache hit");
            return Some(Arc::clone(&entry.models));
        }
    }

    let url = catalog_endpoint(settings)?;
    let mut req = client.get(&url);
    if let Some(k) = settings.credential() {
        req = req.bearer_auth(k);
    }
    let resp = req.send().await.ok()?;
    if !resp.status().is_success() {
        tracing::debug!(status = %resp.status(), url = %url, "openrouter models request failed");
        return None;
    }
    let body: OpenRouterModelsResponse = resp.json().await.ok()?;
    let models = Arc::new(body.data);
    {
        if let Ok(mut guard) = openrouter_catalog_cache().lock() {
            *guard = Some(OpenRouterCatalogEntry {
                key,
                fetched_at: Instant::now(),
                models: Arc::clone(&models),
            });
        }
    }
    Some(models)
}

fn pick_openrouter<'a>(models: &'a [OpenRouterModel], wanted: &str) -> Option<&'a OpenRouterModel> {
    let w = wanted.to_lowercase();
    models
        .iter()
        .find(|m| m.id.to_lowercase() == w)
        .or_else(|| {
            models.iter().find(|m| {
                let id = m.id.to_lowercase();
                id.ends_with(&format!("/{w}"))
            })
        })
}

#[derive(Debug, Deserialize)]
struct AnthropicModelsPage {
    data: Vec<AnthropicModel>,
    #[serde(default)]
    has_more: bool,
}

#[derive(Debug, Deserialize)]
struct AnthropicModel {
    id: String,
    max_input_tokens: Option<u64>,
    /// Present on some API versions; reserved for future output-cap hints.
    #[serde(default)]
    #[allow(dead_code)]
    max_tokens: Option<u64>,
}

async fn fetch_anthropic_catalog(
    client: &reqwest::Client,
    settings: &ResolvedProviderSettings,
) -> Option<Arc<Vec<AnthropicModel>>> {
    let key = cache_key(settings, None)?;
    let api_key = settings.credential()?;
    let ttl = catalog_cache_ttl();
    {
        let guard = anthropic_catalog_cache().lock().ok()?;
        if let Some(entry) = guard.as_ref()
            && entry.key == key
            && !cache_stale(entry.fetched_at, ttl)
        {
            tracing::debug!("anthropic models catalog cache hit");
            return Some(Arc::clone(&entry.models));
        }
    }

    let endpoint = catalog_endpoint(settings)?;
    let mut all: Vec<AnthropicModel> = Vec::new();
    let mut after_id: Option<String> = None;

    loop {
        let mut url = format!("{endpoint}?limit=100");
        if let Some(ref id) = after_id {
            url.push_str("&after_id=");
            url.push_str(id);
        }

        let resp = client
            .get(&url)
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .send()
            .await
            .ok()?;

        if !resp.status().is_success() {
            tracing::debug!(status = %resp.status(), url = %url, "anthropic models request failed");
            return None;
        }

        let page: AnthropicModelsPage = resp.json().await.ok()?;
        if page.data.is_empty() {
            break;
        }

        let cursor = page.data.last().map(|m| m.id.clone());
        all.extend(page.data);

        if !page.has_more {
            break;
        }
        after_id = cursor;
    }

    let models = Arc::new(all);
    {
        if let Ok(mut guard) = anthropic_catalog_cache().lock() {
            *guard = Some(AnthropicCatalogEntry {
                key,
                fetched_at: Instant::now(),
                models: Arc::clone(&models),
            });
        }
    }
    Some(models)
}

fn pick_anthropic<'a>(models: &'a [AnthropicModel], wanted: &str) -> Option<&'a AnthropicModel> {
    let w = wanted.to_lowercase();
    if let Some(m) = models.iter().find(|m| m.id.to_lowercase() == w) {
        return Some(m);
    }
    models.iter().find(|m| {
        let id = m.id.to_lowercase();
        id.starts_with(&w) && (id.len() == w.len() || id.as_bytes().get(w.len()) == Some(&b'-'))
    })
}

fn openai_context_from_catalog(value: &serde_json::Value, model: &str) -> Option<usize> {
    let data = value.get("data")?.as_array()?;
    let w = model.to_lowercase();
    for m in data {
        let id = m.get("id")?.as_str()?.to_lowercase();
        if id != w {
            continue;
        }
        if let Some(cw) = m.get("context_window").and_then(|x| x.as_u64()) {
            return Some(cw as usize);
        }
    }
    None
}

async fn fetch_openai_catalog(
    client: &reqwest::Client,
    settings: &ResolvedProviderSettings,
) -> Option<Arc<serde_json::Value>> {
    let key = cache_key(settings, None)?;
    let api_key = settings.credential()?;
    let ttl = catalog_cache_ttl();
    {
        let guard = openai_catalog_cache().lock().ok()?;
        if let Some(entry) = guard.as_ref()
            && entry.key == key
            && !cache_stale(entry.fetched_at, ttl)
        {
            tracing::debug!("openai models catalog cache hit");
            return Some(Arc::clone(&entry.value));
        }
    }

    let url = catalog_endpoint(settings)?;
    let resp = client.get(&url).bearer_auth(api_key).send().await.ok()?;
    if !resp.status().is_success() {
        tracing::debug!(status = %resp.status(), url = %url, "openai models request failed");
        return None;
    }
    let v: serde_json::Value = resp.json().await.ok()?;
    let value = Arc::new(v);
    {
        if let Ok(mut guard) = openai_catalog_cache().lock() {
            *guard = Some(OpenAiCatalogEntry {
                key,
                fetched_at: Instant::now(),
                value: Arc::clone(&value),
            });
        }
    }
    Some(value)
}

async fn fetch_custom_catalog(
    client: &reqwest::Client,
    settings: &ResolvedProviderSettings,
    protocol: CustomCapabilityAdapter,
) -> Option<Arc<Vec<CustomModelCatalogEntry>>> {
    let key = cache_key(settings, None)?;
    let api_key = settings.credential()?;
    let ttl = catalog_cache_ttl();
    {
        let guard = custom_catalog_cache().lock().ok()?;
        if let Some(entry) = guard.as_ref()
            && entry.key == key
            && !cache_stale(entry.fetched_at, ttl)
        {
            tracing::debug!("custom models catalog cache hit");
            return Some(Arc::clone(&entry.models));
        }
    }

    let base_url = settings.normalized_base_url()?;
    let mut all = Vec::new();
    let mut cursor = None;

    loop {
        let response = protocol
            .models_request(client, &base_url, api_key, cursor.as_deref())
            .ok()?
            .send()
            .await
            .ok()?;
        let status = response.status();
        if !status.is_success() {
            tracing::debug!(status = %status, "custom models request failed");
            return None;
        }
        let body = response.text().await.ok()?;
        let page = protocol.parse_models_page(&body).ok()?;
        all.extend(page.models);
        if !page.has_more {
            break;
        }
        let next_cursor = page.next_cursor?;
        if cursor.as_deref() == Some(next_cursor.as_str()) {
            tracing::debug!("custom models pagination cursor did not advance");
            return None;
        }
        cursor = Some(next_cursor);
    }

    let models = Arc::new(all);
    if let Ok(mut guard) = custom_catalog_cache().lock() {
        *guard = Some(CustomCatalogEntry {
            key,
            fetched_at: Instant::now(),
            models: Arc::clone(&models),
        });
    }
    Some(models)
}

fn pick_custom<'a>(
    models: &'a [CustomModelCatalogEntry],
    wanted: &str,
    protocol: CustomCapabilityAdapter,
) -> Option<&'a CustomModelCatalogEntry> {
    let wanted = wanted.to_lowercase();
    models
        .iter()
        .find(|model| model.id.to_lowercase() == wanted)
        .or_else(|| {
            if protocol == CustomCapabilityAdapter::Anthropic {
                models.iter().find(|model| {
                    let id = model.id.to_lowercase();
                    id.starts_with(&wanted)
                        && (id.len() == wanted.len()
                            || id.as_bytes().get(wanted.len()) == Some(&b'-'))
                })
            } else {
                None
            }
        })
}

/// Fetch available model IDs from the active provider's API.
/// Returns a sorted list of model ID strings. Uses the same cache as context-window lookups.
pub async fn fetch_provider_model_ids(config: &NcaConfig) -> Vec<String> {
    if !config.memory.context.query_provider_models_api {
        return Vec::new();
    }
    if std::env::var("NCA_SKIP_CONTEXT_API").ok().as_deref() == Some("1") {
        return Vec::new();
    }
    let client = match http_client() {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    ProviderCapabilityAdapter::from_config(config)
        .model_ids(&client)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::thread::{self, JoinHandle};

    fn spawn_model_fixtures(responses: Vec<(u16, String)>) -> (String, JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind model fixture");
        let address = listener.local_addr().expect("model fixture address");
        let server = thread::spawn(move || {
            let mut requests = Vec::with_capacity(responses.len());
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().expect("accept model request");
                let mut reader = BufReader::new(stream.try_clone().expect("clone model stream"));
                let mut request = String::new();
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).expect("read model request");
                    request.push_str(&line);
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                }

                let reason = match status {
                    200 => "OK",
                    503 => "Service Unavailable",
                    _ => "Fixture Response",
                };
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream
                    .write_all(response.as_bytes())
                    .expect("write model response");
                requests.push(request);
            }
            requests
        });

        (format!("http://{address}"), server)
    }

    fn spawn_model_fixture(status: u16, body: &str) -> (String, JoinHandle<String>) {
        let (base_url, server) = spawn_model_fixtures(vec![(status, body.to_string())]);
        let single_request = thread::spawn(move || {
            server
                .join()
                .expect("model fixture thread")
                .into_iter()
                .next()
                .expect("model fixture request")
        });
        (base_url, single_request)
    }

    #[test]
    fn openrouter_pick_exact() {
        let models = vec![
            OpenRouterModel {
                id: "openai/gpt-4o".into(),
                context_length: Some(128_000),
            },
            OpenRouterModel {
                id: "other/x".into(),
                context_length: Some(8_000),
            },
        ];
        let m = pick_openrouter(&models, "openai/gpt-4o").unwrap();
        assert_eq!(m.context_length, Some(128_000));
    }

    #[test]
    fn anthropic_pick_prefix() {
        let models = vec![AnthropicModel {
            id: "claude-3-5-sonnet-20241022".into(),
            max_input_tokens: Some(200_000),
            max_tokens: Some(8192),
        }];
        let m = pick_anthropic(&models, "claude-3-5-sonnet").unwrap();
        assert_eq!(m.max_input_tokens, Some(200_000));
    }

    #[test]
    fn openai_parse_context_window_from_cached_json() {
        let v: serde_json::Value = serde_json::json!({
            "data": [
                { "id": "gpt-4o", "context_window": 128000 }
            ]
        });
        assert_eq!(openai_context_from_catalog(&v, "gpt-4o"), Some(128_000));
        assert_eq!(openai_context_from_catalog(&v, "gpt-4o-mini"), None);
    }

    #[test]
    fn custom_catalog_base_url_preserves_optional_v1_suffix() {
        let mut config = NcaConfig::default();
        config.provider.default = ProviderKind::Custom;
        config.provider.custom.base_url = "https://gateway.example/v1/".into();

        assert_eq!(
            config
                .provider
                .active_settings()
                .normalized_base_url()
                .as_deref(),
            Some("https://gateway.example/v1")
        );
    }

    #[test]
    fn provider_settings_cache_identity_redacts_credentials_and_keeps_protocol() {
        let mut config = NcaConfig::default();
        config.provider.default = ProviderKind::Custom;
        config.provider.custom.base_url = "https://gateway.example/tenant/v1".into();
        config.provider.custom.api_key = Some("custom-secret-value".into());
        config.provider.custom.api_key_env = "CUSTOM_TEST_KEY".into();
        config.provider.custom.compatibility = ProviderCompatibility::Anthropic;

        let settings = config.provider.active_settings();
        assert_eq!(settings.model, "custom-model");
        assert_eq!(settings.api_key_env, "CUSTOM_TEST_KEY");
        assert!(settings.credential_present());

        let key = cache_key(&settings, Some("claude-model")).unwrap();
        let rendered = format!("{key:?}");
        assert!(!rendered.contains("custom-secret-value"));
        assert_eq!(key.provider, ProviderKind::Custom);
        assert_eq!(key.compatibility, Some(ProviderCompatibility::Anthropic));
        assert_eq!(key.model.as_deref(), Some("claude-model"));
        assert_eq!(key.credential_tag, Some(api_key_tag("custom-secret-value")));
        assert!(key.endpoint.ends_with("/tenant/v1/models"));
    }

    #[test]
    fn cache_identity_changes_for_endpoint_and_model() {
        let mut config = NcaConfig::default();
        config.provider.default = ProviderKind::Custom;
        config.provider.custom.base_url = "https://gateway.example/one/v1".into();
        config.provider.custom.api_key = Some("cache-key".into());

        let first = config.provider.active_settings();
        let first_key = cache_key(&first, Some("model-a")).unwrap();
        let model_key = cache_key(&first, Some("model-b")).unwrap();

        config.provider.custom.compatibility = ProviderCompatibility::Anthropic;
        let compatibility_key =
            cache_key(&config.provider.active_settings(), Some("model-a")).unwrap();

        config.provider.custom.base_url = "https://gateway.example/two/v1".into();
        let endpoint_key = cache_key(&config.provider.active_settings(), Some("model-a")).unwrap();

        assert_ne!(first_key, model_key);
        assert_ne!(first_key, compatibility_key);
        assert_ne!(first_key, endpoint_key);
    }

    #[test]
    fn capability_dispatch_covers_native_and_custom_protocol_variants() {
        let mut config = NcaConfig::default();

        config.provider.default = ProviderKind::MiniMax;
        assert!(matches!(
            ProviderCapabilityAdapter::from_config(&config),
            ProviderCapabilityAdapter::MiniMax { .. }
        ));

        config.provider.default = ProviderKind::OpenRouter;
        assert!(matches!(
            ProviderCapabilityAdapter::from_config(&config),
            ProviderCapabilityAdapter::OpenRouter { .. }
        ));

        config.provider.default = ProviderKind::OpenAi;
        assert!(matches!(
            ProviderCapabilityAdapter::from_config(&config),
            ProviderCapabilityAdapter::OpenAi { .. }
        ));

        config.provider.default = ProviderKind::Anthropic;
        assert!(matches!(
            ProviderCapabilityAdapter::from_config(&config),
            ProviderCapabilityAdapter::Anthropic { .. }
        ));

        config.provider.default = ProviderKind::Custom;
        config.provider.custom.compatibility = ProviderCompatibility::OpenAi;
        assert!(matches!(
            ProviderCapabilityAdapter::from_config(&config),
            ProviderCapabilityAdapter::Custom { settings, protocol }
                if settings.provider == ProviderKind::Custom
                    && protocol == CustomCapabilityAdapter::OpenAi
        ));
        config.provider.custom.compatibility = ProviderCompatibility::Anthropic;
        assert!(matches!(
            ProviderCapabilityAdapter::from_config(&config),
            ProviderCapabilityAdapter::Custom { settings, protocol }
                if settings.provider == ProviderKind::Custom
                    && protocol == CustomCapabilityAdapter::Anthropic
        ));
    }

    #[tokio::test]
    async fn minimax_catalog_remains_static() {
        let mut config = NcaConfig::default();
        config.provider.default = ProviderKind::MiniMax;

        assert_eq!(
            fetch_provider_model_ids(&config).await,
            vec!["MiniMax-M2.5", "MiniMax-M2.7"]
        );
    }

    #[tokio::test]
    async fn custom_openai_context_lookup_uses_capability_adapter() {
        let (base_url, server) =
            spawn_model_fixture(200, r#"{"data":[{"id":"gpt-4o","context_window":123456}]}"#);
        let mut config = NcaConfig::default();
        config.provider.default = ProviderKind::Custom;
        config.provider.custom.base_url = format!("{base_url}/tenant/v1/");
        config.provider.custom.api_key = Some("custom-openai-context-key".into());
        config.provider.custom.compatibility = ProviderCompatibility::OpenAi;

        let limits = resolve_model_limits(&config, "gpt-4o").await;
        let request = server.join().expect("model fixture thread");

        assert_eq!(limits.context_window, 123_456);
        assert!(request.starts_with("GET /tenant/v1/models HTTP/1.1\r\n"));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer custom-openai-context-key\r\n")
        );
    }

    #[tokio::test]
    async fn custom_anthropic_context_lookup_uses_capability_adapter() {
        let (base_url, server) = spawn_model_fixture(
            200,
            r#"{"data":[{"id":"claude-custom","max_input_tokens":234567}],"has_more":false}"#,
        );
        let mut config = NcaConfig::default();
        config.provider.default = ProviderKind::Custom;
        config.provider.custom.base_url = format!("{base_url}/tenant/v1/");
        config.provider.custom.api_key = Some("custom-anthropic-context-key".into());
        config.provider.custom.compatibility = ProviderCompatibility::Anthropic;

        let limits = resolve_model_limits(&config, "claude-custom").await;
        let request = server.join().expect("model fixture thread");
        let request_lower = request.to_ascii_lowercase();

        assert_eq!(limits.context_window, 234_567);
        assert!(request.starts_with("GET /tenant/v1/models?limit=100 HTTP/1.1\r\n"));
        assert!(request_lower.contains("x-api-key: custom-anthropic-context-key\r\n"));
        assert!(request_lower.contains("anthropic-version: 2023-06-01\r\n"));
    }

    #[tokio::test]
    async fn cache_identity_separates_provider_and_credential() {
        let (base_url, server) = spawn_model_fixtures(vec![
            (
                200,
                r#"{"data":[{"id":"shared-model","context_window":1000}]}"#.into(),
            ),
            (
                200,
                r#"{"data":[{"id":"shared-model","context_window":2000}]}"#.into(),
            ),
            (
                200,
                r#"{"data":[{"id":"shared-model","context_window":3000}]}"#.into(),
            ),
        ]);

        let mut custom = NcaConfig::default();
        custom.provider.default = ProviderKind::Custom;
        custom.provider.custom.base_url = base_url.clone();
        custom.provider.custom.api_key = Some("shared-key".into());
        custom.provider.custom.compatibility = ProviderCompatibility::OpenAi;

        let mut openai = NcaConfig::default();
        openai.provider.default = ProviderKind::OpenAi;
        openai.provider.openai.base_url = base_url;
        openai.provider.openai.api_key = Some("shared-key".into());

        assert_eq!(
            resolve_model_limits(&custom, "shared-model")
                .await
                .context_window,
            1000
        );
        assert_eq!(
            resolve_model_limits(&openai, "shared-model")
                .await
                .context_window,
            2000
        );
        openai.provider.openai.api_key = Some("different-key".into());
        assert_eq!(
            resolve_model_limits(&openai, "shared-model")
                .await
                .context_window,
            3000
        );

        let requests = server.join().expect("model fixture thread");
        assert_eq!(requests.len(), 3);
    }

    #[tokio::test]
    async fn custom_openai_discovery_requests_v1_models_and_parses_ids() {
        let (base_url, server) = spawn_model_fixture(
            200,
            r#"{"data":[{"id":"zeta-model"},{"id":"alpha-model"},{"object":"model"}]}"#,
        );
        let mut config = NcaConfig::default();
        config.provider.default = ProviderKind::Custom;
        config.provider.custom.base_url = format!("{base_url}/zen/v1");
        config.provider.custom.api_key = Some("custom-openai-key".into());
        config.provider.custom.compatibility = ProviderCompatibility::OpenAi;

        let ids = fetch_provider_model_ids(&config).await;
        let request = server.join().expect("model fixture thread");

        assert_eq!(ids, vec!["alpha-model", "zeta-model"]);
        assert!(request.starts_with("GET /zen/v1/models HTTP/1.1\r\n"));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer custom-openai-key\r\n")
        );
    }

    #[tokio::test]
    async fn custom_anthropic_discovery_preserves_versioned_path_prefix_and_parses_ids() {
        let (base_url, server) = spawn_model_fixture(
            200,
            r#"{"data":[{"id":"claude-zeta"},{"id":"claude-alpha"}],"has_more":false}"#,
        );
        let mut config = NcaConfig::default();
        config.provider.default = ProviderKind::Custom;
        config.provider.custom.base_url = format!("{base_url}/zen/v1/");
        config.provider.custom.api_key = Some("custom-anthropic-key".into());
        config.provider.custom.compatibility = ProviderCompatibility::Anthropic;

        let ids = fetch_provider_model_ids(&config).await;
        let request = server.join().expect("model fixture thread");
        let request_lower = request.to_ascii_lowercase();

        assert_eq!(ids, vec!["claude-alpha", "claude-zeta"]);
        assert!(request.starts_with("GET /zen/v1/models?limit=100 HTTP/1.1\r\n"));
        assert!(request_lower.contains("x-api-key: custom-anthropic-key\r\n"));
        assert!(request_lower.contains("anthropic-version: 2023-06-01\r\n"));
    }

    #[tokio::test]
    async fn custom_anthropic_discovery_follows_pagination() {
        let (base_url, server) = spawn_model_fixtures(vec![
            (
                200,
                r#"{"data":[{"id":"claude-alpha"}],"has_more":true}"#.into(),
            ),
            (
                200,
                r#"{"data":[{"id":"claude-zeta"}],"has_more":false}"#.into(),
            ),
        ]);
        let mut config = NcaConfig::default();
        config.provider.default = ProviderKind::Custom;
        config.provider.custom.base_url = base_url;
        config.provider.custom.api_key = Some("custom-anthropic-key".into());
        config.provider.custom.compatibility = ProviderCompatibility::Anthropic;

        let ids = fetch_provider_model_ids(&config).await;
        let requests = server.join().expect("model fixture thread");

        assert_eq!(ids, vec!["claude-alpha", "claude-zeta"]);
        assert_eq!(requests.len(), 2);
        assert!(requests[0].starts_with("GET /v1/models?limit=100 HTTP/1.1\r\n"));
        assert!(
            requests[1].starts_with("GET /v1/models?limit=100&after_id=claude-alpha HTTP/1.1\r\n")
        );
        for request in requests {
            let request_lower = request.to_ascii_lowercase();
            assert!(request_lower.contains("x-api-key: custom-anthropic-key\r\n"));
            assert!(request_lower.contains("anthropic-version: 2023-06-01\r\n"));
        }
    }

    #[tokio::test]
    async fn custom_model_discovery_returns_empty_on_provider_failure() {
        let (base_url, server) = spawn_model_fixture(503, r#"{"error":"unavailable"}"#);
        let mut config = NcaConfig::default();
        config.provider.default = ProviderKind::Custom;
        config.provider.custom.base_url = base_url;
        config.provider.custom.api_key = Some("custom-openai-key".into());

        let ids = fetch_provider_model_ids(&config).await;
        let request = server.join().expect("model fixture thread");

        assert!(ids.is_empty());
        assert!(request.starts_with("GET /v1/models HTTP/1.1\r\n"));
    }
}
