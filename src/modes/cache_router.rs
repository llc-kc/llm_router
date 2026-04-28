use crate::modes::Worker;
use crate::modes::cache_utils::{convert_to_bigram_key, get_hash_str};
use crate::modes::mooncake_client::MooncakeClient;
use crate::modes::req_utils::is_streaming_request;
use crate::modes::router_common::{
    CommonRouter, WorkerConfig, handle_non_streaming_route, handle_streaming_route, select_worker_by_cache_hit_rate,
};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use std::sync::Arc;

/// Cache-based router that routes requests based on KV cache hit rate
pub struct CacheRouter {
    /// Common router functionality
    common_router: CommonRouter,
    /// Worker configurations sorted by max_cache_hit_rate (ascending - lowest threshold first)
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

impl CacheRouter {
    pub fn new(
        worker_configs: Vec<WorkerConfig>,
        mooncake_client: Arc<MooncakeClient>,
        page_size: usize,
        use_eagle: bool,
        add_prefix_tokens: Vec<u32>,
    ) -> Self {
        // Sort by max_cache_hit_rate in ascending order (lowest threshold first)
        let mut configs = worker_configs;
        configs.sort_by(|a, b| a.max_cache_hit_rate.partial_cmp(&b.max_cache_hit_rate).unwrap());

        // If use_eagle is set, multiply page_size by 2
        let page_size = if use_eagle { page_size * 2 } else { page_size };

        Self {
            common_router: CommonRouter::new(),
            worker_configs: configs,
            mooncake_client,
            page_size,
            use_eagle,
            add_prefix_tokens,
        }
    }

    /// Get token IDs from request value
    /// If input_ids is provided in the request, use it directly
    /// Otherwise, tokenize the chat messages
    pub fn get_token_ids_from_value(&self, request_value: &serde_json::Value) -> Result<Vec<u32>, String> {
        self.common_router
            .get_token_ids_from_value(request_value, &self.worker_configs, &self.add_prefix_tokens)
    }

    /// Calculate cache hit rate for given token IDs
    /// Groups tokens by page_size, computes hash for each group, and queries Mooncake
    pub async fn calculate_cache_hit_rate(&self, token_ids: &[u32]) -> Result<f32, String> {
        // to do: set by env var
        let token_len_th = 1;
        if token_ids.len() < token_len_th {
            log::debug!(
                "total_tokens {} < token_len_th {}, set cache hit rate to 0.0",
                token_ids.len(),
                token_len_th
            );
            return Ok(0.0);
        }

        // to do:
        // for V32, we need query k cache and also indexer cache. we can list query indexer cache as to do
        // for MHA, we need query each tp rank

        // Convert tokens to bigram format if use_eagle is enabled
        let processed_tokens: Vec<u32> = if self.use_eagle {
            let i32_tokens: Vec<i32> = token_ids.iter().map(|&t| t as i32).collect();
            let bigram_tokens = convert_to_bigram_key(&i32_tokens);
            bigram_tokens.iter().map(|&t| t as u32).collect()
        } else {
            token_ids.to_vec()
        };
        log::debug!("processed_tokens to compute hash: {:?}", processed_tokens);

        let total_tokens = processed_tokens.len();
        log::debug!("total_tokens: {:?}", total_tokens);

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

        // Use binary search with key_exists to find the maximum consecutive hit count
        // This avoids querying all hashes when there's a miss in the middle
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

    /// Select worker based on cache hit rate
    pub fn select_worker(&self, cache_hit_rate: f32) -> Option<&Worker> {
        select_worker_by_cache_hit_rate(&self.worker_configs, cache_hit_rate)
    }

    /// Find the maximum number of consecutive cache hits using binary search
    /// Uses key_exists to check individual hashes, finding the last consecutive hit
    /// Assumes cache hits are always consecutive from the start (prefix cache property)
    async fn find_max_consecutive_hits(&self, page_hashes: &[String]) -> Result<usize, String> {
        if page_hashes.is_empty() {
            return Ok(0);
        }

        // Binary search to find the boundary between existing and non-existing keys
        // Invariant: all keys in [0, left) exist, keys in [right, n) don't exist
        let mut left = 0usize;
        let mut right = page_hashes.len();

        while left < right {
            let mid = left + (right - left) / 2;

            let key_with_suffix = format!("{}__k", &page_hashes[mid]);
            match self.mooncake_client.key_exists(&key_with_suffix).await {
                Ok(true) => {
                    // mid exists, so all keys before it should also exist (prefix property)
                    left = mid + 1;
                },
                Ok(false) => {
                    // mid doesn't exist, so we need to search in the left half
                    right = mid;
                },
                Err(e) => return Err(format!("Failed to query Mooncake: {}", e)),
            }
        }

        // left is the count of consecutive existing keys
        Ok(left)
    }
}

/// Main entry point for cache-based routing
pub async fn handle_cache_route_mode(
    client: &reqwest::Client,
    cache_router: &CacheRouter,
    endpoint: &str,
    request_value: &serde_json::Value,
) -> Response {
    // Get token IDs
    // If input_ids is provided in the request, use it directly
    // Otherwise, tokenize the chat messages
    let token_ids = match cache_router.get_token_ids_from_value(request_value) {
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

    // Calculate cache hit rate
    let cache_hit_rate = match cache_router.calculate_cache_hit_rate(&token_ids).await {
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

    // Select worker based on cache hit rate
    let worker = match cache_router.select_worker(cache_hit_rate) {
        Some(w) => w,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                axum::Json(serde_json::json!({
                    "error": "No suitable worker found for this cache hit rate"
                })),
            )
                .into_response();
        },
    };

    // Add routing info to response headers (for debugging/monitoring)
    let is_streaming = is_streaming_request(request_value);

    if is_streaming {
        handle_streaming_route(client, worker, endpoint, request_value).await
    } else {
        handle_non_streaming_route(client, worker, endpoint, request_value)
            .await
            .into_response()
    }
}
