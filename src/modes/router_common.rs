use crate::modes::Worker;
use crate::modes::req_utils::process_chat_messages;
use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use dashmap::DashMap;
use futures_util::StreamExt;
use llm_tokenizer::{create_tokenizer_from_file, traits::Tokenizer};
use openai_protocol::chat::ChatCompletionRequest;
use std::io;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;

/// Worker configuration with both token and cache hit rate thresholds
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    pub worker: Worker,
    /// Maximum token count this worker can handle (inclusive)
    pub max_token_threshold: usize,
    /// Maximum cache hit rate this worker can handle (inclusive)
    pub max_cache_hit_rate: f32,
    /// Path to tokenizer model for this worker
    pub tokenizer_path: String,
}

impl WorkerConfig {
    pub fn new(worker: Worker, max_token_threshold: usize, max_cache_hit_rate: f32, tokenizer_path: String) -> Self {
        Self {
            worker,
            max_token_threshold,
            max_cache_hit_rate,
            tokenizer_path,
        }
    }
}

/// Common router functionality for token-based routing
pub struct CommonRouter {
    /// Tokenizer cache: path -> Arc<dyn Tokenizer>
    tokenizer_cache: DashMap<String, Arc<dyn Tokenizer>>,
}

impl CommonRouter {
    pub fn new() -> Self {
        Self {
            tokenizer_cache: DashMap::new(),
        }
    }

    /// Get or create tokenizer for a given path
    pub fn get_tokenizer(&self, path: &str) -> Result<Arc<dyn Tokenizer>, String> {
        if let Some(entry) = self.tokenizer_cache.get(path) {
            return Ok(entry.clone());
        }

        let tokenizer =
            create_tokenizer_from_file(path).map_err(|e| format!("Failed to create tokenizer from {}: {}", path, e))?;

        self.tokenizer_cache.insert(path.to_string(), tokenizer.clone());
        Ok(tokenizer)
    }

    /// Get token IDs for a chat completion request
    /// If input_ids is provided in the request_value, use it directly
    /// Otherwise, tokenize the chat messages
    pub fn get_token_ids(
        &self,
        request: &ChatCompletionRequest,
        worker_configs: &[WorkerConfig],
        add_prefix_tokens: &[u32],
    ) -> Result<Vec<u32>, String> {
        // Use the first worker's tokenizer as default
        let first_config = worker_configs.first().ok_or("No worker configurations available")?;

        let tokenizer = self.get_tokenizer(&first_config.tokenizer_path)?;

        // Process chat messages to get formatted text
        let formatted_text = process_chat_messages(request, tokenizer.as_ref())?;
        log::debug!("Formatted text: {}", formatted_text);

        // Encode and get token IDs
        let encoding = tokenizer
            .encode(&formatted_text, false)
            .map_err(|e| format!("Tokenization failed: {}", e))?;

        let token_ids = encoding.token_ids();

        // Add prefix tokens if configured
        let token_ids = if add_prefix_tokens.is_empty() {
            token_ids.to_vec()
        } else {
            let mut prefixed = add_prefix_tokens.to_vec();
            prefixed.extend_from_slice(token_ids);
            prefixed
        };

        Ok(token_ids)
    }

    /// Get token IDs from request value
    /// If input_ids is provided in the request, use it directly
    /// If text is provided (SGLang /generate endpoint), tokenize the text directly
    /// Otherwise, tokenize the chat messages
    pub fn get_token_ids_from_value(
        &self,
        request_value: &serde_json::Value,
        worker_configs: &[WorkerConfig],
        add_prefix_tokens: &[u32],
    ) -> Result<Vec<u32>, String> {
        // Check if input_ids is provided directly in the request
        if let Some(input_ids) = request_value.get("input_ids") {
            if let Some(ids_array) = input_ids.as_array() {
                let token_ids: Result<Vec<u32>, String> = ids_array
                    .iter()
                    .map(|v| {
                        v.as_u64()
                            .map(|n| n as u32)
                            .ok_or_else(|| format!("Invalid input_id: {:?}", v))
                    })
                    .collect();
                let token_ids = token_ids?;
                return Ok(token_ids);
            }
        }

        // Check if text is provided (SGLang /generate endpoint format)
        if let Some(text) = request_value.get("text") {
            if let Some(text_str) = text.as_str() {
                return self.get_token_ids_from_text(text_str, worker_configs, add_prefix_tokens);
            }
        }

        // Fallback to tokenizing chat messages
        let chat_request: ChatCompletionRequest = match serde_json::from_value(request_value.clone()) {
            Ok(req) => req,
            Err(e) => return Err(format!("Failed to parse chat completion request: {}", e)),
        };

        self.get_token_ids(&chat_request, worker_configs, add_prefix_tokens)
    }

    /// Get token IDs from plain text (for SGLang /generate endpoint)
    fn get_token_ids_from_text(
        &self,
        text: &str,
        worker_configs: &[WorkerConfig],
        add_prefix_tokens: &[u32],
    ) -> Result<Vec<u32>, String> {
        // Use the first worker's tokenizer as default
        let first_config = worker_configs.first().ok_or("No worker configurations available")?;
        let tokenizer = self.get_tokenizer(&first_config.tokenizer_path)?;

        // Encode the text directly
        let encoding = tokenizer
            .encode(text, false)
            .map_err(|e| format!("Tokenization failed: {}", e))?;

        let token_ids = encoding.token_ids();

        // Add prefix tokens if configured
        let token_ids = if add_prefix_tokens.is_empty() {
            token_ids.to_vec()
        } else {
            let mut prefixed = add_prefix_tokens.to_vec();
            prefixed.extend_from_slice(token_ids);
            prefixed
        };

        Ok(token_ids)
    }
}

impl Default for CommonRouter {
    fn default() -> Self {
        Self::new()
    }
}

/// Select worker based on token count
pub fn select_worker_by_token_count(worker_configs: &[WorkerConfig], token_count: usize) -> Option<&Worker> {
    // Find the first worker whose max_token_threshold >= token_count
    // Workers should be sorted by threshold in ascending order
    for config in worker_configs {
        if token_count <= config.max_token_threshold {
            log::info!("Selected worker: {:?}, token count: {}", config.worker, token_count);
            return Some(&config.worker);
        }
    }

    // If no worker can handle this token count, return the last one (highest threshold)
    worker_configs.last().map(|c| &c.worker)
}

/// Select worker based on cache hit rate
pub fn select_worker_by_cache_hit_rate(worker_configs: &[WorkerConfig], cache_hit_rate: f32) -> Option<&Worker> {
    // Find the first worker whose max_cache_hit_rate >= cache_hit_rate
    // Workers are sorted by max_cache_hit_rate in ascending order
    for config in worker_configs {
        if cache_hit_rate <= config.max_cache_hit_rate {
            log::info!(
                "Selected worker: {:?}, cache hit rate: {:.4}, max_cache_hit_rate: {:.4}",
                config.worker,
                cache_hit_rate,
                config.max_cache_hit_rate
            );
            return Some(&config.worker);
        }
    }

    // If no worker can handle this cache hit rate, return the last one (highest max_cache_hit_rate)
    worker_configs.last().map(|c| &c.worker)
}

/// Handle non-streaming routing
pub async fn handle_non_streaming_route(
    client: &reqwest::Client,
    worker: &Worker,
    endpoint: &str,
    request_value: &serde_json::Value,
) -> axum::Json<serde_json::Value> {
    let url = worker.to_url(endpoint);
    log::info!("route non-streaming request to {}", url);

    match client.post(&url).json(request_value).send().await {
        Ok(resp) => {
            if let Ok(json) = resp.json::<serde_json::Value>().await {
                axum::Json(json)
            } else {
                axum::Json(serde_json::json!({
                    "error": "Failed to parse response from Worker"
                }))
            }
        },
        Err(e) => {
            log::error!("Error forwarding to {}: {}", url, e);
            axum::Json(serde_json::json!({
                "error": format!("Failed to forward request: {}", e)
            }))
        },
    }
}

/// Handle streaming routing
pub async fn handle_streaming_route(
    client: &reqwest::Client,
    worker: &Worker,
    endpoint: &str,
    request_value: &serde_json::Value,
) -> Response {
    let url = worker.to_url(endpoint);
    log::info!("route streaming request to {}", url);

    let response = match client.post(&url).json(request_value).send().await {
        Ok(resp) => resp,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                axum::Json(serde_json::json!({
                    "error": format!("Failed to forward request: {}", e)
                })),
            )
                .into_response();
        },
    };

    let status = response.status();
    let status_code = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

    if !status.is_success() {
        let error_body = response.text().await.unwrap_or_default();
        return (
            status_code,
            axum::Json(serde_json::json!({
                "error": format!("Upstream error: {}", error_body)
            })),
        )
            .into_response();
    }

    // Get response headers
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );

    // Create streaming response
    let stream = response.bytes_stream();
    let (tx, rx) = mpsc::unbounded_channel::<Result<Bytes, io::Error>>();

    tokio::spawn(async move {
        let mut stream = stream;
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(bytes) => {
                    if tx.send(Ok(bytes)).is_err() {
                        break;
                    }
                },
                Err(e) => {
                    let _ = tx.send(Err(io::Error::other(e)));
                    break;
                },
            }
        }
    });

    let body_stream = UnboundedReceiverStream::new(rx);
    let body = Body::from_stream(body_stream);

    let mut response = Response::new(body);
    *response.status_mut() = status_code;
    *response.headers_mut() = response_headers;

    response
}
