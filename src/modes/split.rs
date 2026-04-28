use crate::modes::Worker;
use crate::modes::req_utils::is_streaming_request;
use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::StreamExt;
use rand::Rng;
use std::io;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;

pub fn select_worker(workers: &[Worker], strategy: &str, round_robin_counter: &Arc<Mutex<usize>>) -> Worker {
    match strategy {
        "round_robin" => {
            let mut counter = round_robin_counter.lock().unwrap();
            let index = *counter % workers.len();
            *counter += 1;
            workers[index].clone()
        },
        "random" => {
            let index = rand::rng().random_range(0..workers.len());
            workers[index].clone()
        },
        _ => workers[0].clone(),
    }
}

/// Handle non-streaming split mode
async fn handle_non_streaming_route(
    client: &reqwest::Client,
    workers: &[Worker],
    endpoint: &str,
    request_value: &serde_json::Value,
    strategy: &str,
    round_robin_counter: &Arc<Mutex<usize>>,
) -> axum::Json<serde_json::Value> {
    let selected_worker = select_worker(workers, strategy, round_robin_counter);
    let url = selected_worker.to_url(endpoint);
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

/// Handle streaming split mode
async fn handle_streaming_route(
    client: &reqwest::Client,
    workers: &[Worker],
    endpoint: &str,
    request_value: &serde_json::Value,
    strategy: &str,
    round_robin_counter: &Arc<Mutex<usize>>,
) -> Response {
    if workers.is_empty() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(serde_json::json!({
                "error": "No workers configured"
            })),
        )
            .into_response();
    }

    let selected_worker = select_worker(workers, strategy, round_robin_counter);
    let url = selected_worker.to_url(endpoint);
    log::info!("route streaming request to {}", url);

    // Send streaming request
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

pub async fn handle_split_mode(
    client: &reqwest::Client,
    workers: &[Worker],
    endpoint: &str,
    request_value: &serde_json::Value,
    strategy: &str,
    round_robin_counter: &Arc<Mutex<usize>>,
) -> Response {
    if is_streaming_request(request_value) {
        handle_streaming_route(client, workers, endpoint, request_value, strategy, round_robin_counter).await
    } else {
        handle_non_streaming_route(client, workers, endpoint, request_value, strategy, round_robin_counter)
            .await
            .into_response()
    }
}
