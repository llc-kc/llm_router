use axum::{Router, extract::State, routing::post, serve};
use clap::Parser;
use openai_protocol::chat::ChatCompletionRequest;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Target {
    ip: String,
    port: u16,
}

impl Target {
    fn new(ip: String, port: u16) -> Self {
        Self { ip, port }
    }

    fn to_url(&self, endpoint: &str) -> String {
        format!("http://{}:{}{}", self.ip, self.port, endpoint)
    }
}

struct AppState {
    targets: Arc<Mutex<Vec<Target>>>,
    endpoint: String,
    mode: String,
    strategy: String,
    round_robin_counter: Arc<Mutex<usize>>,
}

async fn handle_mirror_mode(
    client: &reqwest::Client,
    targets: &[Target],
    endpoint: &str,
    request_value: &serde_json::Value,
) -> axum::Json<serde_json::Value> {
    // 镜像模式：全部分流到每个目标，只返回第一个结果
    let mut responses = Vec::new();
    for target in targets {
        let url = target.to_url(endpoint);
        match client.post(&url).json(request_value).send().await {
            Ok(resp) => {
                if let Ok(json) = resp.json::<serde_json::Value>().await {
                    responses.push(json);
                }
            },
            Err(e) => {
                eprintln!("Error forwarding to {}: {}", url, e);
            },
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

fn select_target(
    targets: &[Target],
    strategy: &str,
    round_robin_counter: &Arc<Mutex<usize>>,
) -> Target {
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

async fn handle_split_mode(
    client: &reqwest::Client,
    targets: &[Target],
    endpoint: &str,
    request_value: &serde_json::Value,
    strategy: &str,
    round_robin_counter: &Arc<Mutex<usize>>,
) -> axum::Json<serde_json::Value> {
    // 分流模式：根据策略选择目标
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

    // 直接将请求转换为 JSON 对象
    let request_value = serde_json::to_value(request).unwrap();

    match state.mode.as_str() {
        "mirror" => handle_mirror_mode(&client, &targets, &endpoint, &request_value).await,
        "split" => handle_split_mode(&client, &targets, &endpoint, &request_value, &state.strategy, &state.round_robin_counter).await,
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
    // 初始化日志
    tracing_subscriber::fmt::init();

    // 解析命令行参数
    let args = Args::parse();

    // 解析目标地址
    let targets = parse_targets(&args.targets);
    let endpoint = args.endpoint.clone();
    let mode = args.mode.clone();
    let strategy = args.strategy.clone();

    // 创建应用状态
    let state = Arc::new(AppState {
        targets: Arc::new(Mutex::new(targets)),
        endpoint: endpoint.clone(),
        mode: mode.clone(),
        strategy: strategy.clone(),
        round_robin_counter: Arc::new(Mutex::new(0)),
    });

    // 构建路由
    let app = Router::new()
        // 处理LLM请求
        .route(&endpoint, post(handle_chat_completions))
        // 管理路由API
        .route("/api/targets", post(add_target).get(list_targets).delete(remove_target))
        .with_state(state);

    // 启动服务器
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
