use crate::modes::Target;
use rand::Rng;
use std::sync::{Arc, Mutex};

pub fn select_target(targets: &[Target], strategy: &str, round_robin_counter: &Arc<Mutex<usize>>) -> Target {
    match strategy {
        "round_robin" => {
            let mut counter = round_robin_counter.lock().unwrap();
            let index = *counter % targets.len();
            *counter += 1;
            targets[index].clone()
        },
        "random" => {
            let index = rand::thread_rng().gen_range(0..targets.len());
            targets[index].clone()
        },
        _ => targets[0].clone(),
    }
}

pub async fn handle_split_mode(
    client: &reqwest::Client,
    targets: &[Target],
    endpoint: &str,
    request_value: &serde_json::Value,
    strategy: &str,
    round_robin_counter: &Arc<Mutex<usize>>,
) -> axum::Json<serde_json::Value> {
    let selected_target = select_target(targets, strategy, round_robin_counter);

    let url = selected_target.to_url(endpoint);
    match client.post(&url).json(request_value).send().await {
        Ok(resp) => {
            if let Ok(json) = resp.json::<serde_json::Value>().await {
                axum::Json(json)
            } else {
                axum::Json(serde_json::json!({
                    "error": "Failed to parse response from target"
                }))
            }
        },
        Err(e) => {
            eprintln!("Error forwarding to {}: {}", url, e);
            axum::Json(serde_json::json!({
                "error": format!("Failed to forward request: {}", e)
            }))
        },
    }
}
