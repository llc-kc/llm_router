use axum::{Router, extract::State, routing::post, serve};
use clap::Parser;
use openai_protocol::chat::ChatCompletionRequest;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

mod modes;
use modes::{mirror, split, Target};

#[derive(Parser, Debug)]
#[command(version, about = "LLM请求镜像流量router工具")]
struct Args {
    /// 输入请求的端口
    #[arg(long, default_value = "8080")]
    port: u16,

    /// 输入请求的接口路径
    #[arg(long, default_value = "/v1/chat/completions")]
    endpoint: String,

    /// 输出请求的IP和端口，格式为ip:port，多个用逗号分隔
    #[arg(long, default_value = "")]
    targets: String,

    /// 运行模式：mirror（镜像模式）或 split（分流模式）
    #[arg(long, default_value = "mirror")]
    mode: String,

    /// 分流模式下的负载均衡策略：round_robin或random
    #[arg(long, default_value = "round_robin")]
    strategy: String,
}

struct AppState {
    targets: Arc<Mutex<Vec<Target>>>,
    endpoint: String,
    mode: String,
    strategy: String,
    round_robin_counter: Arc<Mutex<usize>>,
}

async fn handle_chat_completions(
    State(state): State<Arc<AppState>>,
    axum::Json(request): axum::Json<ChatCompletionRequest>,
) -> impl axum::response::IntoResponse {
    let targets = state.targets.lock().unwrap().clone();
    if targets.is_empty() {
        return axum::Json(serde_json::json!({
            "error": "No targets configured"
        }));
    }

    let client = reqwest::Client::new();
    let endpoint = state.endpoint.clone();

    let request_value = serde_json::to_value(request).unwrap();

    match state.mode.as_str() {
        "mirror" => mirror::handle_mirror_mode(&client, &targets, &endpoint, &request_value).await,
        "split" => split::handle_split_mode(&client, &targets, &endpoint, &request_value, &state.strategy, &state.round_robin_counter).await,
        _ => axum::Json(serde_json::json!({
            "error": "Invalid mode"
        })),
    }
}

async fn add_target(
    State(state): State<Arc<AppState>>,
    axum::Json(target): axum::Json<Target>,
) -> impl axum::response::IntoResponse {
    let mut targets = state.targets.lock().unwrap();
    targets.push(target);
    axum::Json(serde_json::json!({
        "status": "success",
        "message": "Target added"
    }))
}

async fn remove_target(
    State(state): State<Arc<AppState>>,
    axum::Json(target): axum::Json<Target>,
) -> impl axum::response::IntoResponse {
    let mut targets = state.targets.lock().unwrap();
    let initial_len = targets.len();
    targets.retain(|t| !(t.ip == target.ip && t.port == target.port));
    if targets.len() < initial_len {
        axum::Json(serde_json::json!({
            "status": "success",
            "message": "Target removed"
        }))
    } else {
        axum::Json(serde_json::json!({
            "status": "error",
            "message": "Target not found"
        }))
    }
}

async fn list_targets(State(state): State<Arc<AppState>>) -> impl axum::response::IntoResponse {
    let targets = state.targets.lock().unwrap().clone();
    axum::Json(serde_json::json!({
        "status": "success",
        "targets": targets
    }))
}

fn parse_targets(targets_str: &str) -> Vec<Target> {
    let mut targets = Vec::new();
    for target_str in targets_str.split(',') {
        let target_str = target_str.trim();
        if !target_str.is_empty() {
            if let Some((ip, port)) = target_str.split_once(':') {
                if let Ok(port) = port.parse::<u16>() {
                    targets.push(Target::new(ip.to_string(), port));
                }
            }
        }
    }
    targets
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    let targets = parse_targets(&args.targets);
    let endpoint = args.endpoint.clone();
    let mode = args.mode.clone();
    let strategy = args.strategy.clone();

    let state = Arc::new(AppState {
        targets: Arc::new(Mutex::new(targets)),
        endpoint: endpoint.clone(),
        mode: mode.clone(),
        strategy: strategy.clone(),
        round_robin_counter: Arc::new(Mutex::new(0)),
    });

    let app = Router::new()
        .route(&endpoint, post(handle_chat_completions))
        .route("/api/targets", post(add_target).get(list_targets).delete(remove_target))
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], args.port));
    println!("LLM Router started at http://{}", addr);
    println!("Mode: {}", mode);
    if mode == "split" {
        println!("Strategy: {}", strategy);
    }
    println!("Endpoint: {}", endpoint);
    println!("Initial targets: {:?}", parse_targets(&args.targets));

    let listener = tokio::net::TcpListener::bind(addr).await?;
    serve(listener, app.into_make_service()).await?;

    Ok(())
}
