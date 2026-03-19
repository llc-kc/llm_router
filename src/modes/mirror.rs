use crate::modes::Target;

pub async fn handle_mirror_mode(
    client: &reqwest::Client,
    targets: &[Target],
    endpoint: &str,
    request_value: &serde_json::Value,
) -> axum::Json<serde_json::Value> {
    let mut responses = Vec::new();
    for target in targets {
        let url = target.to_url(endpoint);
        match client.post(&url).json(request_value).send().await {
            Ok(resp) => {
                if let Ok(json) = resp.json::<serde_json::Value>().await {
                    responses.push(json);
                }
            }
            Err(e) => {
                eprintln!("Error forwarding to {}: {}", url, e);
            }
        }
    }

    if let Some(first_response) = responses.first() {
        axum::Json(first_response.clone())
    } else {
        axum::Json(serde_json::json!({
            "error": "No successful responses from targets"
        }))
    }
}
