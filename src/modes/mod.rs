pub mod cache_router;
pub mod cache_router_pd;
pub mod cache_utils;
pub mod mirror;
pub mod mooncake_client;
pub mod req_utils;
pub mod router_common;
pub mod split;
pub mod token_router;
pub mod utils;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Worker {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_length: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_hit_rate: Option<f64>,
}

impl Worker {
    pub fn to_url(&self, endpoint: &str) -> String {
        format!("{}{}", self.url, endpoint)
    }
}
