#![allow(dead_code)]
//! Mooncake HTTP Client - Client for interacting with Mooncake Store

use log::debug;
use reqwest::{Client, StatusCode, Url};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::time::Duration;
use thiserror::Error;

/// Result type alias for operations
pub type Result<T> = std::result::Result<T, ClientError>;

/// Error types for Mooncake HTTP Client
#[derive(Debug, Error)]
pub enum ClientError {
    #[error("HTTP error: {0}")]
    Http(reqwest::Error),

    #[error("URL parse error: {0}")]
    UrlParse(#[from] url::ParseError),

    #[error("Server error {status}: {message}")]
    ServerError { status: u16, message: String },

    #[error("Client error {status}: {message}")]
    ClientError { status: u16, message: String },

    #[error("JSON parse error: {0}")]
    JsonParse(#[from] serde_json::Error),

    #[error("Invalid response: {0}")]
    InvalidResponse(String),

    #[error("Request timeout")]
    Timeout,
}

impl From<reqwest::Error> for ClientError {
    fn from(err: reqwest::Error) -> Self {
        if err.is_timeout() {
            ClientError::Timeout
        } else {
            ClientError::Http(err)
        }
    }
}

/// Result of checking if a key exists
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExistenceResult {
    Exists,
    NotExists,
}

impl ExistenceResult {
    /// Returns true if the key exists
    #[allow(dead_code)]
    pub fn exists(&self) -> bool {
        matches!(self, ExistenceResult::Exists)
    }
}

impl From<bool> for ExistenceResult {
    fn from(value: bool) -> Self {
        if value {
            ExistenceResult::Exists
        } else {
            ExistenceResult::NotExists
        }
    }
}

impl fmt::Display for ExistenceResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExistenceResult::Exists => write!(f, "EXISTS"),
            ExistenceResult::NotExists => write!(f, "NOT FOUND"),
        }
    }
}

/// Individual key info from batch query
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyInfo {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<ReplicaDescriptor>>,
}

/// Replica descriptor (BufferDescriptor from server)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplicaDescriptor {
    #[serde(rename = "size_")]
    pub size: u64,
    #[serde(rename = "buffer_address_")]
    pub buffer_address: u64,
    #[serde(rename = "protocol_")]
    pub protocol: String,
    #[serde(rename = "transport_endpoint_")]
    pub transport_endpoint: String,
}

/// Response from batch query keys endpoint
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchQueryResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<HashMap<String, KeyInfo>>,
}

impl BatchQueryResponse {
    pub fn all_exists(&self) -> HashMap<String, ExistenceResult> {
        let mut results = HashMap::new();
        if let Some(data) = &self.data {
            for (key, info) in data {
                let exists = if info.ok {
                    ExistenceResult::Exists
                } else {
                    ExistenceResult::NotExists
                };
                results.insert(key.clone(), exists);
            }
        }
        results
    }
}

/// Health check response
#[derive(Debug, Clone)]
pub struct HealthResponse {
    pub healthy: bool,
    pub message: String,
}

/// HTTP client for interacting with Mooncake Store
#[derive(Debug, Clone)]
pub struct MooncakeClient {
    http_client: Client,
    base_url: Url,
}

impl MooncakeClient {
    pub fn new(base_url: &str) -> Result<Self> {
        let http_client = Client::builder().timeout(Duration::from_secs(30)).build()?;
        let base_url = Url::parse(base_url)?;
        Ok(Self { http_client, base_url })
    }

    pub async fn key_exists(&self, key: &str) -> Result<bool> {
        let url = self.base_url.join("/query_key")?;
        debug!("Checking if key '{}' exists at {}", key, url);

        let response = self.http_client.get(url).query(&[("key", key)]).send().await?;

        match response.status() {
            StatusCode::OK => Ok(true),
            StatusCode::NOT_FOUND => Ok(false),
            status if status.is_server_error() => {
                let text = response.text().await.unwrap_or_default();
                Err(ClientError::ServerError {
                    status: status.as_u16(),
                    message: text,
                })
            },
            status if status.is_client_error() => {
                let text = response.text().await.unwrap_or_default();
                Err(ClientError::ClientError {
                    status: status.as_u16(),
                    message: text,
                })
            },
            status => {
                let text = response.text().await.unwrap_or_default();
                Err(ClientError::InvalidResponse(format!(
                    "Unexpected status {}: {}",
                    status.as_u16(),
                    text
                )))
            },
        }
    }

    pub async fn batch_keys_exist(&self, keys: &[&str]) -> Result<HashMap<String, ExistenceResult>> {
        if keys.is_empty() {
            return Ok(HashMap::new());
        }

        let keys_param = keys.join(",");
        // Manually construct URL to avoid URL-encoding the commas in the keys parameter
        let url_str = format!("{}?keys={}", self.base_url.join("/batch_query_keys")?, keys_param);
        let url = Url::parse(&url_str)?;
        debug!("Batch checking keys [{}] at {}", keys_param, url);

        let response = self.http_client.get(url).send().await?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(ClientError::ServerError {
                status: status.as_u16(),
                message: text,
            });
        }

        let batch_response: BatchQueryResponse = response.json().await?;

        if !batch_response.success {
            return Err(ClientError::ServerError {
                status: 500,
                message: batch_response.error.unwrap_or_else(|| "Unknown error".to_string()),
            });
        }

        Ok(batch_response.all_exists())
    }

    pub async fn get_all_keys(&self) -> Result<Vec<String>> {
        let url = self.base_url.join("/get_all_keys")?;
        debug!("Getting all keys from {}", url);

        let response = self.http_client.get(url).send().await?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(ClientError::ServerError {
                status: status.as_u16(),
                message: text,
            });
        }

        let text = response.text().await?;
        let keys: Vec<String> = text
            .lines()
            .filter(|line| !line.is_empty())
            .map(|s| s.to_string())
            .collect();

        Ok(keys)
    }

    pub async fn get_all_segments(&self) -> Result<Vec<String>> {
        let url = self.base_url.join("/get_all_segments")?;
        let response = self.http_client.get(url).send().await?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(ClientError::ServerError {
                status: status.as_u16(),
                message: text,
            });
        }

        let text = response.text().await?;
        let segments: Vec<String> = text
            .lines()
            .filter(|line| !line.is_empty())
            .map(|s| s.to_string())
            .collect();

        Ok(segments)
    }

    pub async fn health_check(&self) -> Result<HealthResponse> {
        let url = self.base_url.join("/health")?;
        let response = self.http_client.get(url).send().await?;
        let status = response.status();
        let text = response.text().await?;

        Ok(HealthResponse {
            healthy: status == StatusCode::OK,
            message: text,
        })
    }

    pub async fn get_metrics(&self) -> Result<String> {
        let url = self.base_url.join("/metrics")?;
        let response = self.http_client.get(url).send().await?;
        let content = response.text().await?;
        Ok(content)
    }
}
