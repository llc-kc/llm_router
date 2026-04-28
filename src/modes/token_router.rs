use crate::modes::Worker;
use crate::modes::req_utils::is_streaming_request;
use crate::modes::router_common::{
    CommonRouter, WorkerConfig, handle_non_streaming_route, handle_streaming_route, select_worker_by_token_count,
};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

/// Token-based router that routes requests based on token count
pub struct TokenRouter {
    /// Common router functionality
    common_router: CommonRouter,
    /// Worker configurations sorted by threshold (ascending)
    worker_configs: Vec<WorkerConfig>,
}

impl TokenRouter {
    pub fn new(worker_configs: Vec<WorkerConfig>) -> Self {
        Self {
            common_router: CommonRouter::new(),
            worker_configs,
        }
    }

    /// Get token IDs from request value
    /// If input_ids is provided in the request, use it directly
    /// Otherwise, tokenize the chat messages
    pub fn get_token_ids_from_value(&self, request_value: &serde_json::Value) -> Result<Vec<u32>, String> {
        self.common_router
            .get_token_ids_from_value(request_value, &self.worker_configs, &[])
    }

    /// Count tokens from token IDs array
    pub fn count_tokens(&self, token_ids: &[u32]) -> usize {
        token_ids.len()
    }

    /// Select worker based on token count
    pub fn select_worker(&self, token_count: usize) -> Option<&Worker> {
        select_worker_by_token_count(&self.worker_configs, token_count)
    }
}

/// Main entry point for token-based routing
pub async fn handle_token_route_mode(
    client: &reqwest::Client,
    token_router: &TokenRouter,
    endpoint: &str,
    request_value: &serde_json::Value,
) -> Response {
    // Get token IDs and count tokens
    // If input_ids is provided in the request, use it directly
    // Otherwise, tokenize the chat messages
    let token_ids = match token_router.get_token_ids_from_value(request_value) {
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
    let token_count = token_router.count_tokens(&token_ids);
    log::debug!("token_ids: {:?}", token_ids);
    log::info!("Token count: {}", token_ids.len());

    // Select worker based on token count
    let worker = match token_router.select_worker(token_count) {
        Some(w) => w,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                axum::Json(serde_json::json!({
                    "error": "No suitable worker found for this token count"
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
