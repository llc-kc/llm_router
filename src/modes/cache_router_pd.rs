use crate::modes::Worker;
use crate::modes::cache_utils::{convert_to_bigram_key, get_hash_str};
use crate::modes::mooncake_client::MooncakeClient;
use crate::modes::req_utils::is_streaming_request;
use crate::modes::router_common::{CommonRouter, WorkerConfig};
use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::StreamExt;
use std::io;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;

/// Cache-based PD (Prefill and Decode) router
/// This router uses two workers:
/// - worker0: Low cache hit rate threshold, used for prefill (generates 1 token)
/// - worker1: High cache hit rate threshold, used for full generation
pub struct CacheRouterPd {
    /// Common router functionality
    common_router: CommonRouter,
    /// Worker configurations (exactly 2 workers: worker0 and worker1)
    worker_configs: Vec<WorkerConfig>,
    /// Mooncake client for querying cache existence
    mooncake_client: Arc<MooncakeClient>,
    /// Page size for token grouping (default: 64)
    page_size: usize,
    /// Use eagle mode for bigram token processing
    use_eagle: bool,
    /// Prefix tokens to add before input tokens when calculating cache hit rate
    add_prefix_tokens: Vec<u32>,
}

impl CacheRouterPd {
    pub fn new(
        worker_configs: Vec<WorkerConfig>,
        mooncake_client: Arc<MooncakeClient>,
        page_size: usize,
        use_eagle: bool,
        add_prefix_tokens: Vec<u32>,
    ) -> Result<Self, String> {
        // Validate that exactly 2 workers are provided
        if worker_configs.len() != 2 {
            return Err(format!(
                "CacheRouterPd requires exactly 2 workers, but got {}",
                worker_configs.len()
            ));
        }

        // Sort by max_cache_hit_rate in ascending order (lowest threshold first)
        // worker0 should have lower threshold, worker1 should have higher threshold
        let mut configs = worker_configs;
        configs.sort_by(|a, b| a.max_cache_hit_rate.partial_cmp(&b.max_cache_hit_rate).unwrap());

        // If use_eagle is set, multiply page_size by 2
        let page_size = if use_eagle { page_size * 2 } else { page_size };

        Ok(Self {
            common_router: CommonRouter::new(),
            worker_configs: configs,
            mooncake_client,
            page_size,
            use_eagle,
            add_prefix_tokens,
        })
    }

    /// Get token IDs from request value
    /// If input_ids is provided in the request, use it directly
    /// Otherwise, tokenize the chat messages
    pub fn get_token_ids_from_value(&self, request_value: &serde_json::Value) -> Result<Vec<u32>, String> {
        self.common_router
            .get_token_ids_from_value(request_value, &self.worker_configs, &self.add_prefix_tokens)
    }

    /// Calculate page hashes for given token IDs
    /// Groups tokens by page_size, computes hash for each group with chaining
    pub fn calculate_page_hashes(&self, token_ids: &[u32]) -> (Vec<String>, Vec<u32>) {
        // Convert tokens to bigram format if use_eagle is enabled
        let processed_tokens: Vec<u32> = if self.use_eagle {
            let i32_tokens: Vec<i32> = token_ids.iter().map(|&t| t as i32).collect();
            let bigram_tokens = convert_to_bigram_key(&i32_tokens);
            bigram_tokens.iter().map(|&t| t as u32).collect()
        } else {
            token_ids.to_vec()
        };
        log::debug!("processed_tokens to compute hash: {:?}", processed_tokens);

        // Group token IDs by page_size, using previous chunk's hash for chaining
        let mut page_hashes: Vec<String> = Vec::new();
        let mut prior_hash: Option<String> = None;
        for chunk in processed_tokens.chunks(self.page_size) {
            let hash = get_hash_str(chunk, prior_hash.as_deref());
            prior_hash = Some(hash.clone());
            page_hashes.push(hash);
        }

        // Discard the last page hash if the last chunk is not full
        if !page_hashes.is_empty() && processed_tokens.len() % self.page_size != 0 {
            page_hashes.pop();
        }

        log::debug!("page_hashes: {:?}", page_hashes);

        (page_hashes, processed_tokens)
    }

    /// Calculate cache hit rate using precomputed page hashes and processed tokens
    /// Queries Mooncake to find maximum consecutive hits
    pub async fn calculate_cache_hit_rate(
        &self,
        token_ids: &[u32],
        page_hashes: &[String],
        processed_tokens: &[u32],
    ) -> Result<f32, String> {
        let token_len_th = 1;
        if token_ids.len() < token_len_th {
            log::debug!(
                "total_tokens {} < token_len_th {}, set cache hit rate to 0.0",
                token_ids.len(),
                token_len_th
            );
            return Ok(0.0);
        }

        let total_tokens = processed_tokens.len();
        log::debug!("total_tokens: {:?}", total_tokens);

        // Use binary search with key_exists to find the maximum consecutive hit count
        let hit_count = self.find_max_consecutive_hits(&page_hashes).await?;
        log::info!("hit_count: {:?}", hit_count);

        // Calculate hit rate: (hit_pages * page_size) / total_tokens
        let hit_tokens = hit_count * self.page_size;
        let hit_rate = (hit_tokens as f32 / total_tokens as f32).min(1.0f32);

        log::info!(
            "Cache hit pages: {}/{}, Hit tokens: {}/{}, Hit rate: {:.4}",
            hit_count,
            page_hashes.len(),
            hit_tokens,
            total_tokens,
            hit_rate
        );

        Ok(hit_rate)
    }

    /// Get worker0 (low cache hit rate threshold worker)
    pub fn get_worker0(&self) -> &Worker {
        &self.worker_configs[0].worker
    }

    /// Get worker1 (high cache hit rate threshold worker)
    pub fn get_worker1(&self) -> &Worker {
        &self.worker_configs[1].worker
    }

    /// Get worker0's cache hit rate threshold
    pub fn get_worker0_threshold(&self) -> f32 {
        self.worker_configs[0].max_cache_hit_rate
    }

    /// Find the maximum number of consecutive cache hits using binary search
    async fn find_max_consecutive_hits(&self, page_hashes: &[String]) -> Result<usize, String> {
        if page_hashes.is_empty() {
            return Ok(0);
        }

        let mut left = 0usize;
        let mut right = page_hashes.len();

        while left < right {
            let mid = left + (right - left) / 2;

            let key_with_suffix = format!("{}__k", &page_hashes[mid]);
            match self.mooncake_client.key_exists(&key_with_suffix).await {
                Ok(true) => {
                    left = mid + 1;
                },
                Ok(false) => {
                    right = mid;
                },
                Err(e) => return Err(format!("Failed to query Mooncake: {}", e)),
            }
        }

        Ok(left)
    }

    /// Check if the last N chunks are cached (default: 8)
    /// Returns true if at least one of the last chunks exists
    async fn check_req_last_chunks_cached(
        &self,
        page_hashes: &[String],
        num_chunks: Option<usize>,
    ) -> Result<bool, String> {
        let num_chunks = num_chunks.unwrap_or(8);
        if page_hashes.is_empty() {
            return Ok(false);
        }

        let start = page_hashes.len().saturating_sub(num_chunks);
        let last_chunks = &page_hashes[start..];
        let keys: Vec<String> = last_chunks.iter().map(|h| format!("{}__k", h)).collect();
        let key_refs: Vec<&str> = keys.iter().map(|k| k.as_str()).collect();

        log::debug!("Checking last {} chunks: {:?}", key_refs.len(), key_refs);

        match self.mooncake_client.batch_keys_exist(&key_refs).await {
            Ok(results) => {
                let any_exists = results
                    .values()
                    .any(|r| matches!(r, crate::modes::mooncake_client::ExistenceResult::Exists));
                log::debug!("Last chunks cached: {}", any_exists);
                Ok(any_exists)
            },
            Err(e) => Err(format!("Failed to batch query Mooncake: {}", e)),
        }
    }

    /// Wait for cache to be ready
    /// Polls check_req_last_chunks_cached at the specified interval until timeout
    async fn wait_for_cache(
        &self,
        page_hashes: &[String],
        check_interval_ms: Option<u64>,
        timeout_ms: Option<u64>,
    ) -> Result<bool, String> {
        let check_interval = std::time::Duration::from_millis(check_interval_ms.unwrap_or(80));
        // to do: determine timeout based on req length
        let timeout = std::time::Duration::from_millis(timeout_ms.unwrap_or(500));
        let start_time = std::time::Instant::now();

        log::info!(
            "Waiting for cache with interval={:?}, timeout={:?}",
            check_interval,
            timeout
        );

        loop {
            if start_time.elapsed() >= timeout {
                log::warn!("Timeout waiting for cache after {:?}", timeout);
                return Ok(false);
            }

            match self.check_req_last_chunks_cached(page_hashes, None).await {
                Ok(true) => {
                    log::info!("Cache ready after {:?}", start_time.elapsed());
                    return Ok(true);
                },
                Ok(false) => {
                    tokio::time::sleep(check_interval).await;
                },
                Err(e) => {
                    log::error!("Error checking cache: {}", e);
                    return Err(e);
                },
            }
        }
    }
}

/// Create a prefill request by setting max_new_tokens to 0
/// This will generate exactly 1 token (the first token)
fn create_prefill_request(request_value: &serde_json::Value) -> serde_json::Value {
    let mut prefill_request = request_value.clone();

    // Check if this is a SGLang /generate request (has sampling_params or text field)
    let is_sglang_format = prefill_request.get("sampling_params").is_some()
        || prefill_request.get("text").is_some()
        || prefill_request.get("input_ids").is_some();

    // Check if this is an OpenAI /v1/chat/completions request (has messages field)
    let is_openai_format = prefill_request.get("messages").is_some();

    if is_sglang_format {
        // SGLang /generate format: set sampling_params.max_new_tokens = 0
        if let Some(params) = prefill_request.get_mut("sampling_params") {
            if let Some(obj) = params.as_object_mut() {
                obj.insert("max_new_tokens".to_string(), serde_json::json!(0));
            }
        } else {
            // If sampling_params doesn't exist, create it
            prefill_request["sampling_params"] = serde_json::json!({
                "max_new_tokens": 0
            });
        }
    } else if is_openai_format {
        // OpenAI /v1/chat/completions format: set max_tokens = 0
        // This field controls the number of tokens to generate
        prefill_request["max_tokens"] = serde_json::json!(0);
        // Also set max_completion_tokens if present (newer OpenAI API)
        prefill_request["max_completion_tokens"] = serde_json::json!(0);
    } else {
        // Fallback: try both approaches
        if let Some(params) = prefill_request.get_mut("sampling_params") {
            if let Some(obj) = params.as_object_mut() {
                obj.insert("max_new_tokens".to_string(), serde_json::json!(0));
            }
        }
        prefill_request["max_tokens"] = serde_json::json!(0);
        prefill_request["max_completion_tokens"] = serde_json::json!(0);
    }

    prefill_request
}

/// Forward non-streaming request to worker1 and return the response
async fn forward_to_worker1(
    client: &reqwest::Client,
    worker1: &Worker,
    endpoint: &str,
    request_value: &serde_json::Value,
) -> axum::Json<serde_json::Value> {
    let worker1_url = worker1.to_url(endpoint);
    log::info!("Routing request to worker1: {}", worker1_url);

    match client.post(&worker1_url).json(request_value).send().await {
        Ok(resp) => {
            if let Ok(json) = resp.json::<serde_json::Value>().await {
                axum::Json(json)
            } else {
                axum::Json(serde_json::json!({
                    "error": "Failed to parse response from worker1"
                }))
            }
        },
        Err(e) => {
            log::error!("Error forwarding to worker1 {}: {}", worker1_url, e);
            axum::Json(serde_json::json!({
                "error": format!("Failed to forward request to worker1: {}", e)
            }))
        },
    }
}

/// Handle non-streaming request with PD (Prefill and Decode) routing
async fn handle_non_streaming_pd_route(
    client: &reqwest::Client,
    router: &CacheRouterPd,
    endpoint: &str,
    request_value: &serde_json::Value,
) -> axum::Json<serde_json::Value> {
    let worker0 = router.get_worker0();
    let worker1 = router.get_worker1();
    let worker0_threshold = router.get_worker0_threshold();

    // Get token IDs and calculate cache hit rate
    let token_ids = match router.get_token_ids_from_value(request_value) {
        Ok(ids) => ids,
        Err(e) => {
            return axum::Json(serde_json::json!({
                "error": format!("Failed to get token IDs: {}", e)
            }));
        },
    };
    log::debug!("token_ids: {:?}", token_ids);
    log::info!("Token count: {}", token_ids.len());

    // Calculate page hashes first
    let (page_hashes, processed_tokens) = router.calculate_page_hashes(&token_ids);
    log::debug!("calculated page_hashes: {:?}", page_hashes);
    log::debug!("processed_tokens count: {}", processed_tokens.len());

    // Then calculate cache hit rate
    let cache_hit_rate = match router
        .calculate_cache_hit_rate(&token_ids, &page_hashes, &processed_tokens)
        .await
    {
        Ok(rate) => rate,
        Err(e) => {
            return axum::Json(serde_json::json!({
                "error": format!("Failed to calculate cache hit rate: {}", e)
            }));
        },
    };
    log::info!("Cache hit rate: {:.4}", cache_hit_rate);

    // Route based on cache hit rate
    if cache_hit_rate <= worker0_threshold {
        // Low cache hit rate: prefill from worker0, then decode from worker1
        // Step 1: Send prefill request to worker0 with max_new_tokens=0
        let prefill_request = create_prefill_request(request_value);

        let worker0_url = worker0.to_url(endpoint);
        let worker1_prefetch_url = format!("{}{}/prefetch", worker1.url, endpoint);
        log::info!(
            "Cache hit rate {:.4} <= threshold {:.4}, routing to worker0 {} first, and will route to worker 1 again",
            cache_hit_rate,
            worker0_threshold,
            worker0_url
        );

        // Concurrently send prefill to worker0 and prefetch to worker1
        let prefill_future = async {
            match client.post(&worker0_url).json(&prefill_request).send().await {
                Ok(resp) => {
                    if !resp.status().is_success() {
                        let error_body = resp.text().await.unwrap_or_default();
                        return Err(format!("Worker0 prefill failed: {}", error_body));
                    }
                    // Discard the response, we only care that the cache is warmed up
                    let _ = resp.json::<serde_json::Value>().await;
                    log::info!("Prefill to worker0 completed");
                    Ok(())
                },
                Err(e) => {
                    log::error!("Prefill to worker0 {} failed: {}", worker0_url, e);
                    Err(format!("Failed to prefill to worker0: {}", e))
                },
            }
        };

        let prefetch_future = async move {
            log::info!("Sending prefetch request to worker1: {}", worker1_prefetch_url);
            match client.post(&worker1_prefetch_url).json(request_value).send().await {
                Ok(resp) => {
                    if !resp.status().is_success() {
                        let error_body = resp.text().await.unwrap_or_default();
                        log::warn!("Worker1 prefetch failed: {}", error_body);
                    } else {
                        log::info!("Prefetch to worker1 completed");
                    }
                },
                Err(e) => {
                    log::warn!("Prefetch to worker1 {} failed: {}", worker1_prefetch_url, e);
                },
            }
        };

        // Run both requests concurrently
        let (prefill_result, _) = tokio::join!(prefill_future, prefetch_future);

        if let Err(e) = prefill_result {
            return axum::Json(serde_json::json!({
                "error": e
            }));
        }
    }

    log::info!("Waiting for cache to be ready before routing to worker1");
    if let Err(e) = router.wait_for_cache(&page_hashes, None, None).await {
        log::error!("Error waiting for cache: {}", e);
    }

    forward_to_worker1(client, worker1, endpoint, request_value).await
}

/// Handle streaming request with PD (Prefill and Decode) routing
async fn handle_streaming_pd_route(
    client: &reqwest::Client,
    router: &CacheRouterPd,
    endpoint: &str,
    request_value: &serde_json::Value,
) -> Response {
    let worker0 = router.get_worker0();
    let worker1 = router.get_worker1();
    let worker0_threshold = router.get_worker0_threshold();

    // Get token IDs and calculate cache hit rate
    let token_ids = match router.get_token_ids_from_value(request_value) {
        Ok(ids) => ids,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(serde_json::json!({
                    "error": format!("Failed to get token IDs: {}", e)
                })),
            )
                .into_response();
        },
    };
    log::debug!("token_ids: {:?}", token_ids);
    log::info!("Token count: {}", token_ids.len());

    // Calculate page hashes first
    let (page_hashes, processed_tokens) = router.calculate_page_hashes(&token_ids);
    log::debug!("calculated page_hashes: {:?}", page_hashes);
    log::debug!("processed_tokens count: {}", processed_tokens.len());

    // Then calculate cache hit rate
    let cache_hit_rate = match router
        .calculate_cache_hit_rate(&token_ids, &page_hashes, &processed_tokens)
        .await
    {
        Ok(rate) => rate,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(serde_json::json!({
                    "error": format!("Failed to calculate cache hit rate: {}", e)
                })),
            )
                .into_response();
        },
    };
    log::info!("Cache hit rate: {:.4}", cache_hit_rate);

    // Route based on cache hit rate
    if cache_hit_rate <= worker0_threshold {
        // Low cache hit rate: prefill from worker0, then decode from worker1
        log::info!(
            "Cache hit rate {:.4} <= threshold {:.4}, using PD routing (worker0 -> worker1)",
            cache_hit_rate,
            worker0_threshold
        );

        // Step 1: Send prefill request to worker0 with max_new_tokens=0
        let prefill_request = create_prefill_request(request_value);
        let worker0_url = worker0.to_url(endpoint);
        let worker1_prefetch_url = format!("{}{}/prefetch", worker1.url, endpoint);
        log::info!("Step 1: Prefill to worker0: {}", worker0_url);

        // Concurrently send prefill to worker0 and prefetch to worker1
        let prefill_future = async {
            match client.post(&worker0_url).json(&prefill_request).send().await {
                Ok(resp) => {
                    if !resp.status().is_success() {
                        let error_body = resp.text().await.unwrap_or_default();
                        return Err(format!("Worker0 prefill failed: {}", error_body));
                    }
                    // Discard the response
                    let _ = resp.json::<serde_json::Value>().await;
                    log::info!("Prefill to worker0 completed");
                    Ok(())
                },
                Err(e) => {
                    log::error!("Error prefill to worker0 {}: {}", worker0_url, e);
                    Err(format!("Failed to prefill to worker0: {}", e))
                },
            }
        };

        let prefetch_future = async move {
            log::info!("Sending prefetch request to worker1: {}", worker1_prefetch_url);
            match client.post(&worker1_prefetch_url).json(request_value).send().await {
                Ok(resp) => {
                    if !resp.status().is_success() {
                        let error_body = resp.text().await.unwrap_or_default();
                        log::warn!("Worker1 prefetch failed: {}", error_body);
                    } else {
                        log::info!("Prefetch to worker1 completed");
                    }
                },
                Err(e) => {
                    log::warn!("Prefetch to worker1 {} failed: {}", worker1_prefetch_url, e);
                },
            }
        };

        // Run both requests concurrently
        let (prefill_result, _) = tokio::join!(prefill_future, prefetch_future);

        if let Err(e) = prefill_result {
            return (
                StatusCode::BAD_GATEWAY,
                axum::Json(serde_json::json!({
                    "error": e
                })),
            )
                .into_response();
        }
    }

    log::info!("Waiting for cache to be ready before routing to worker1");
    if let Err(e) = router.wait_for_cache(&page_hashes, None, None).await {
        log::error!("Error waiting for cache: {}", e);
    }

    let worker1_url = worker1.to_url(endpoint);
    log::info!("Routing to worker1: {}", worker1_url);
    forward_streaming_request(client, worker1, endpoint, request_value).await
}

/// Forward streaming request to a worker
async fn forward_streaming_request(
    client: &reqwest::Client,
    worker: &Worker,
    endpoint: &str,
    request_value: &serde_json::Value,
) -> Response {
    let url = worker.to_url(endpoint);

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

/// Main entry point for cache-based PD routing
pub async fn handle_cache_route_pd_mode(
    client: &reqwest::Client,
    cache_router_pd: &CacheRouterPd,
    endpoint: &str,
    request_value: &serde_json::Value,
) -> Response {
    let is_streaming = is_streaming_request(request_value);

    if is_streaming {
        handle_streaming_pd_route(client, cache_router_pd, endpoint, request_value).await
    } else {
        handle_non_streaming_pd_route(client, cache_router_pd, endpoint, request_value)
            .await
            .into_response()
    }
}
