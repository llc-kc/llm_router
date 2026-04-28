use crate::modes::Worker;
use crate::modes::req_utils::is_streaming_request;
use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::StreamExt;
use std::io;
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;

/// Handle non-streaming mirror mode
/// Concurrently forwards requests to all workers and returns the first successful response
async fn handle_non_streaming_mirror(
    client: &reqwest::Client,
    workers: &[Worker],
    endpoint: &str,
    request_value: &serde_json::Value,
) -> axum::Json<serde_json::Value> {
    use futures_util::future::join_all;

    if workers.is_empty() {
        return axum::Json(serde_json::json!({
            "error": "No workers configured"
        }));
    }

    // Create concurrent tasks for all workers
    let tasks: Vec<_> = workers
        .iter()
        .map(|worker| {
            let client = client.clone();
            let url = worker.to_url(endpoint);
            let request_value = request_value.clone();

            async move {
                match client.post(&url).json(&request_value).send().await {
                    Ok(resp) => {
                        if let Ok(json) = resp.json::<serde_json::Value>().await {
                            Some(json)
                        } else {
                            log::error!("Error parsing JSON response from {}", url);
                            None
                        }
                    },
                    Err(e) => {
                        log::error!("Error forwarding to {}: {}", url, e);
                        None
                    },
                }
            }
        })
        .collect();

    // Wait for all tasks to complete
    let results = join_all(tasks).await;

    // Return the first successful response
    for result in results {
        if let Some(json) = result {
            return axum::Json(json);
        }
    }

    // No successful responses
    axum::Json(serde_json::json!({
        "error": "No successful responses from workers"
    }))
}

/// Handle streaming mirror mode
/// For streaming requests, we forward to all workers but only return the first worker's stream
async fn handle_streaming_mirror(
    client: &reqwest::Client,
    workers: &[Worker],
    endpoint: &str,
    request_value: &serde_json::Value,
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

    // For streaming, we forward to the first worker and return its stream
    // Other workers receive the request but their responses are ignored
    let primary_worker = &workers[0];
    let url = primary_worker.to_url(endpoint);

    // Clone for background tasks to forward to other workers
    let workers_for_background: Vec<Worker> = workers.iter().skip(1).cloned().collect();
    let client_clone = client.clone();
    let request_value_clone = request_value.clone();
    let endpoint_clone = endpoint.to_string();

    // Spawn background tasks to forward to other workers concurrently (fire and forget)
    // Must consume the response body to prevent server from aborting the request
    for worker in workers_for_background {
        let client = client_clone.clone();
        let request_value = request_value_clone.clone();
        let endpoint = endpoint_clone.clone();

        tokio::spawn(async move {
            let url = worker.to_url(&endpoint);
            if let Ok(resp) = client.post(&url).json(&request_value).send().await {
                // Consume the response body to keep the connection flowing
                // Use chunk() to read and discard data without buffering entire response
                let mut stream = resp.bytes_stream();
                while let Some(_chunk) = stream.next().await {}
            }
        });
    }

    // Send streaming request to primary worker
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

pub async fn handle_mirror_mode(
    client: &reqwest::Client,
    workers: &[Worker],
    endpoint: &str,
    request_value: &serde_json::Value,
) -> Response {
    if is_streaming_request(request_value) {
        handle_streaming_mirror(client, workers, endpoint, request_value).await
    } else {
        handle_non_streaming_mirror(client, workers, endpoint, request_value)
            .await
            .into_response()
    }
}
