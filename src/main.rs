use axum::{
    Router,
    extract::State,
    response::{IntoResponse, Response},
    routing::{get, post},
    serve,
};
use clap::Parser;
use llm_cahr::logger;
use openai_protocol::chat::ChatCompletionRequest;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

mod modes;
use cache_router::CacheRouter;
use cache_router_pd::CacheRouterPd;
use modes::router_common::WorkerConfig;
use modes::{Worker, cache_router, cache_router_pd, mirror, split, token_router};
use token_router::TokenRouter;

#[derive(Parser, Debug)]
#[command(version, about = "LLM请求镜像流量router工具")]
struct Args {
    /// llm_cahr 输入请求的端口
    #[arg(long, default_value = "8080")]
    port: u16,

    /// Worker配置，JSON格式数组字符串
    /// 例如: [{"url":"http://127.0.0.1:8000"},{"url":"http://127.0.0.1:8001","token_length":2048}]
    #[arg(long, default_value = "")]
    workers: String,

    /// 运行模式：mirror（镜像模式）、split（分流模式）、token_route（基于token数量的路由模式）、cache_route（基于KV cache命中率的路由模式）或 cache_route_pd（基于KV cache命中率的路由模式，支持PD分离）
    #[arg(long, default_value = "mirror")]
    mode: String,

    /// 分流模式下的负载均衡策略：round_robin或random
    #[arg(long, default_value = "round_robin")]
    strategy: String,

    /// Mooncake服务器URL，用于cache_route模式
    #[arg(long, default_value = "")]
    mooncake_url: String,

    /// Page size for token grouping in cache_route mode (default: 64)
    #[arg(long, default_value = "64")]
    page_size: usize,

    /// Use eagle mode for bigram token processing in cache_route mode
    #[arg(long, action = clap::ArgAction::SetTrue)]
    use_eagle: bool,

    /// Tokenizer模型文件路径，用于token_route模式和cache_route模式
    #[arg(long, default_value = "")]
    tokenizer_path: String,

    /// 添加到token前面的前缀token，逗号分割的整数向量。用于cache_route模式
    /// 处理因为tokenizer拼接请求时没有加一些特殊token的情况，例如DeepSeek V3.2需要加一个0的token前缀
    #[arg(long, default_value = "")]
    add_prefix_tokens: String,

    /// 日志级别: error, warn, info, debug, trace (默认: info)
    /// 也可以通过环境变量 RUST_LOG 设置
    #[arg(long, default_value = "info")]
    log_level: String,
}

struct AppState {
    workers: Arc<Mutex<Vec<Worker>>>,
    mode: String,
    strategy: String,
    round_robin_counter: Arc<Mutex<usize>>,
    token_router: Option<Arc<TokenRouter>>,
    cache_router: Option<Arc<CacheRouter>>,
    cache_router_pd: Option<Arc<CacheRouterPd>>,
}

/// 处理 /generate 请求
/// SGLang 风格的生成接口
async fn handle_generate(
    State(state): State<Arc<AppState>>,
    axum::Json(request): axum::Json<serde_json::Value>,
) -> Response {
    let client = reqwest::Client::new();
    let endpoint = "/generate";

    match state.mode.as_str() {
        "token_route" => {
            // Token-based routing mode
            if let Some(ref token_router) = state.token_router {
                token_router::handle_token_route_mode(&client, token_router, endpoint, &request).await
            } else {
                axum::Json(serde_json::json!({
                    "error": "Token router not configured"
                }))
                .into_response()
            }
        },
        "cache_route" => {
            // Cache-based routing mode
            if let Some(ref cache_router) = state.cache_router {
                cache_router::handle_cache_route_mode(&client, cache_router, endpoint, &request).await
            } else {
                axum::Json(serde_json::json!({
                    "error": "Cache router not configured"
                }))
                .into_response()
            }
        },
        "cache_route_pd" => {
            // Cache-based PD routing mode
            if let Some(ref cache_router_pd) = state.cache_router_pd {
                cache_router_pd::handle_cache_route_pd_mode(&client, cache_router_pd, endpoint, &request).await
            } else {
                axum::Json(serde_json::json!({
                    "error": "Cache router PD not configured"
                }))
                .into_response()
            }
        },
        _ => {
            // Other modes (mirror, split)
            let workers = state.workers.lock().unwrap().clone();
            if workers.is_empty() {
                return axum::Json(serde_json::json!({
                    "error": "No workers configured"
                }))
                .into_response();
            }

            match state.mode.as_str() {
                "mirror" => mirror::handle_mirror_mode(&client, &workers, endpoint, &request).await,
                "split" => {
                    split::handle_split_mode(
                        &client,
                        &workers,
                        endpoint,
                        &request,
                        &state.strategy,
                        &state.round_robin_counter,
                    )
                    .await
                },
                _ => axum::Json(serde_json::json!({
                    "error": "Invalid mode"
                }))
                .into_response(),
            }
        },
    }
}

/// 处理 /v1/chat/completions 请求
/// OpenAI 风格的聊天补全接口
async fn handle_chat_completions(
    State(state): State<Arc<AppState>>,
    axum::Json(request): axum::Json<ChatCompletionRequest>,
) -> Response {
    let client = reqwest::Client::new();
    let endpoint = "/v1/chat/completions";
    let request_value = serde_json::to_value(request).unwrap();

    match state.mode.as_str() {
        "token_route" => {
            // Token-based routing mode
            if let Some(ref token_router) = state.token_router {
                token_router::handle_token_route_mode(&client, token_router, endpoint, &request_value).await
            } else {
                axum::Json(serde_json::json!({
                    "error": "Token router not configured"
                }))
                .into_response()
            }
        },
        "cache_route" => {
            // Cache-based routing mode
            if let Some(ref cache_router) = state.cache_router {
                cache_router::handle_cache_route_mode(&client, cache_router, endpoint, &request_value).await
            } else {
                axum::Json(serde_json::json!({
                    "error": "Cache router not configured"
                }))
                .into_response()
            }
        },
        "cache_route_pd" => {
            // Cache-based PD routing mode
            if let Some(ref cache_router_pd) = state.cache_router_pd {
                cache_router_pd::handle_cache_route_pd_mode(&client, cache_router_pd, endpoint, &request_value).await
            } else {
                axum::Json(serde_json::json!({
                    "error": "Cache router PD not configured"
                }))
                .into_response()
            }
        },
        _ => {
            // Other modes (mirror, split)
            let workers = state.workers.lock().unwrap().clone();
            if workers.is_empty() {
                return axum::Json(serde_json::json!({
                    "error": "No workers configured"
                }))
                .into_response();
            }

            match state.mode.as_str() {
                "mirror" => mirror::handle_mirror_mode(&client, &workers, endpoint, &request_value).await,
                "split" => {
                    split::handle_split_mode(
                        &client,
                        &workers,
                        endpoint,
                        &request_value,
                        &state.strategy,
                        &state.round_robin_counter,
                    )
                    .await
                },
                _ => axum::Json(serde_json::json!({
                    "error": "Invalid mode"
                }))
                .into_response(),
            }
        },
    }
}

/// 处理 /v1/completions 请求
/// OpenAI 风格的文本补全接口
async fn handle_completions(
    State(state): State<Arc<AppState>>,
    axum::Json(request): axum::Json<serde_json::Value>,
) -> Response {
    let workers = state.workers.lock().unwrap().clone();
    if workers.is_empty() {
        return axum::Json(serde_json::json!({
            "error": "No workers configured"
        }))
        .into_response();
    }

    let client = reqwest::Client::new();
    let endpoint = "/v1/completions";

    match state.mode.as_str() {
        "mirror" => mirror::handle_mirror_mode(&client, &workers, endpoint, &request).await,
        "split" => {
            split::handle_split_mode(
                &client,
                &workers,
                endpoint,
                &request,
                &state.strategy,
                &state.round_robin_counter,
            )
            .await
        },
        _ => axum::Json(serde_json::json!({
            "error": "Invalid mode"
        }))
        .into_response(),
    }
}

/// 处理 /v1/models 请求
/// 从第一个 worker 获取模型列表并返回
async fn handle_models(State(state): State<Arc<AppState>>) -> impl axum::response::IntoResponse {
    let workers = state.workers.lock().unwrap().clone();
    if workers.is_empty() {
        return axum::Json(serde_json::json!({
            "error": "No workers configured"
        }));
    }

    let client = reqwest::Client::new();
    // 从第一个 worker 获取模型列表
    let first_worker = &workers[0];
    let url = first_worker.to_url("/v1/models");

    match client.get(&url).send().await {
        Ok(response) => {
            if response.status().is_success() {
                match response.json::<serde_json::Value>().await {
                    Ok(json) => axum::Json(json),
                    Err(e) => axum::Json(serde_json::json!({
                        "error": format!("Failed to parse models response: {}", e)
                    })),
                }
            } else {
                axum::Json(serde_json::json!({
                    "error": format!("Worker returned status: {}", response.status())
                }))
            }
        },
        Err(e) => axum::Json(serde_json::json!({
            "error": format!("Failed to fetch models from worker: {}", e)
        })),
    }
}

async fn add_worker(
    State(state): State<Arc<AppState>>,
    axum::Json(worker): axum::Json<Worker>,
) -> impl axum::response::IntoResponse {
    let mut workers = state.workers.lock().unwrap();
    workers.push(worker);
    axum::Json(serde_json::json!({
        "status": "success",
        "message": "Worker added"
    }))
}

async fn remove_worker(
    State(state): State<Arc<AppState>>,
    axum::Json(worker): axum::Json<Worker>,
) -> impl axum::response::IntoResponse {
    let mut workers = state.workers.lock().unwrap();
    let initial_len = workers.len();
    workers.retain(|t| t.url != worker.url);
    if workers.len() < initial_len {
        axum::Json(serde_json::json!({
            "status": "success",
            "message": "Worker removed"
        }))
    } else {
        axum::Json(serde_json::json!({
            "status": "error",
            "message": "Worker not found"
        }))
    }
}

async fn list_workers(State(state): State<Arc<AppState>>) -> impl axum::response::IntoResponse {
    let workers = state.workers.lock().unwrap().clone();
    axum::Json(serde_json::json!({
        "status": "success",
        "workers": workers
    }))
}

fn parse_workers(workers_str: &str) -> Vec<Worker> {
    if workers_str.trim().is_empty() {
        return Vec::new();
    }

    match serde_json::from_str::<Vec<Worker>>(workers_str) {
        Ok(workers) => workers,
        Err(e) => {
            log::error!("Error parsing workers JSON: {}", e);
            Vec::new()
        },
    }
}

/// Convert Workers to WorkerConfigs for token_route mode
fn workers_to_token_configs(workers: &[Worker], tokenizer_path: &str) -> Vec<WorkerConfig> {
    let mut configs = Vec::new();
    for worker in workers {
        let threshold = worker.token_length.unwrap_or(100000000);
        configs.push(WorkerConfig::new(
            worker.clone(),
            threshold,
            1.0, // max_cache_hit_rate not used in token_route mode
            tokenizer_path.to_string(),
        ));
    }
    // Sort by threshold in ascending order
    configs.sort_by_key(|c| c.max_token_threshold);
    configs
}

/// Convert Workers to WorkerConfigs for cache_route mode
fn workers_to_cache_configs(workers: &[Worker], tokenizer_path: &str) -> Vec<WorkerConfig> {
    let mut configs = Vec::new();
    for worker in workers {
        let max_cache_hit_rate = worker.cache_hit_rate.unwrap_or(1.0);
        configs.push(WorkerConfig::new(
            worker.clone(),
            100000000, // max_token_threshold not used in cache_route mode
            max_cache_hit_rate as f32,
            tokenizer_path.to_string(),
        ));
    }
    // Sort by max_cache_hit_rate in ascending order (lowest threshold first)
    // Note: CacheRouter::new will also sort, but we sort here for consistency
    configs.sort_by(|a, b| a.max_cache_hit_rate.partial_cmp(&b.max_cache_hit_rate).unwrap());
    configs
}

/// Parse add_prefix_tokens string (comma-separated integers) into Vec<u32>
fn parse_prefix_tokens(tokens_str: &str) -> Vec<u32> {
    if tokens_str.trim().is_empty() {
        return Vec::new();
    }

    tokens_str
        .split(',')
        .filter_map(|s| s.trim().parse::<u32>().ok())
        .collect()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    // 初始化日志系统
    let log_level = args.log_level.parse().unwrap_or(log::LevelFilter::Info);
    logger::init_logger(Some(log_level));

    let workers = parse_workers(&args.workers);
    let mode = args.mode.clone();
    let strategy = args.strategy.clone();

    // Initialize token router if in token_route mode
    let token_router = if mode == "token_route" {
        if args.tokenizer_path.is_empty() {
            log::error!("Error: token_route mode requires --tokenizer-path argument");
            std::process::exit(1);
        }
        let token_configs = workers_to_token_configs(&workers, &args.tokenizer_path);
        if token_configs.is_empty() {
            log::error!("Error: token_route mode requires at least one worker configured");
            std::process::exit(1);
        }
        log::info!("Token worker configurations:");
        for config in &token_configs {
            log::info!(
                "  - {} (max tokens: {}, tokenizer: {})",
                config.worker.url,
                config.max_token_threshold,
                config.tokenizer_path
            );
        }
        Some(Arc::new(TokenRouter::new(token_configs)))
    } else {
        None
    };

    // Initialize cache router if in cache_route mode
    let cache_router = if mode == "cache_route" {
        if args.tokenizer_path.is_empty() {
            log::error!("Error: cache_route mode requires --tokenizer-path argument");
            std::process::exit(1);
        }
        if args.mooncake_url.is_empty() {
            log::error!("Error: cache_route mode requires --mooncake-url argument");
            std::process::exit(1);
        }
        let cache_configs = workers_to_cache_configs(&workers, &args.tokenizer_path);
        if cache_configs.is_empty() {
            log::error!("Error: cache_route mode requires at least one worker configured");
            std::process::exit(1);
        }
        log::info!("Cache worker configurations:");
        for config in &cache_configs {
            log::info!(
                "  - {} (max cache hit rate: {:.2}, tokenizer: {})",
                config.worker.url,
                config.max_cache_hit_rate,
                config.tokenizer_path
            );
        }
        let mooncake_client = Arc::new(
            modes::mooncake_client::MooncakeClient::new(&args.mooncake_url).expect("Failed to create Mooncake client"),
        );
        log::info!("Mooncake client initialized with URL: {}", args.mooncake_url);
        log::info!("Page size: {} (use_eagle: {})", args.page_size, args.use_eagle);
        let prefix_tokens = parse_prefix_tokens(&args.add_prefix_tokens);
        if !prefix_tokens.is_empty() {
            log::info!("Add prefix tokens: {:?}", prefix_tokens);
        }
        Some(Arc::new(CacheRouter::new(
            cache_configs,
            mooncake_client,
            args.page_size,
            args.use_eagle,
            prefix_tokens,
        )))
    } else {
        None
    };

    // Initialize cache router PD if in cache_route_pd mode
    let cache_router_pd = if mode == "cache_route_pd" {
        if args.tokenizer_path.is_empty() {
            log::error!("Error: cache_route_pd mode requires --tokenizer-path argument");
            std::process::exit(1);
        }
        if args.mooncake_url.is_empty() {
            log::error!("Error: cache_route_pd mode requires --mooncake-url argument");
            std::process::exit(1);
        }
        let cache_configs = workers_to_cache_configs(&workers, &args.tokenizer_path);
        if cache_configs.len() != 2 {
            log::error!(
                "Error: cache_route_pd mode requires exactly 2 workers, but got {}",
                cache_configs.len()
            );
            std::process::exit(1);
        }
        log::info!("Cache PD worker configurations:");
        for (i, config) in cache_configs.iter().enumerate() {
            log::info!(
                "  - worker{}: {} (max cache hit rate: {:.2}, tokenizer: {})",
                i,
                config.worker.url,
                config.max_cache_hit_rate,
                config.tokenizer_path
            );
        }
        let mooncake_client = Arc::new(
            modes::mooncake_client::MooncakeClient::new(&args.mooncake_url).expect("Failed to create Mooncake client"),
        );
        log::info!("Mooncake client initialized with URL: {}", args.mooncake_url);
        log::info!("Page size: {} (use_eagle: {})", args.page_size, args.use_eagle);
        let prefix_tokens = parse_prefix_tokens(&args.add_prefix_tokens);
        if !prefix_tokens.is_empty() {
            log::info!("Add prefix tokens: {:?}", prefix_tokens);
        }
        Some(Arc::new(
            CacheRouterPd::new(
                cache_configs,
                mooncake_client,
                args.page_size,
                args.use_eagle,
                prefix_tokens,
            )
            .expect("Failed to create CacheRouterPd"),
        ))
    } else {
        None
    };

    let state = Arc::new(AppState {
        workers: Arc::new(Mutex::new(workers)),
        mode: mode.clone(),
        strategy: strategy.clone(),
        round_robin_counter: Arc::new(Mutex::new(0)),
        token_router,
        cache_router,
        cache_router_pd,
    });

    // 自动注册路由端点，无需用户设置参数
    let app = Router::new()
        .route("/generate", post(handle_generate))
        .route("/v1/chat/completions", post(handle_chat_completions))
        .route("/v1/completions", post(handle_completions))
        .route("/v1/models", get(handle_models))
        .route("/api/workers", post(add_worker).get(list_workers).delete(remove_worker))
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], args.port));
    log::info!("LLM Router started at http://{}", addr);
    log::info!("Mode: {}", mode);
    if mode == "split" {
        log::info!("Strategy: {}", strategy);
    }
    log::info!("Available endpoints:");
    log::info!("  - POST /generate");
    log::info!("  - POST /v1/chat/completions");
    log::info!("  - POST /v1/completions");
    log::info!("  - GET  /v1/models");
    log::info!("  - POST/GET/DELETE /api/workers");
    log::info!("Initial workers: {:?}", parse_workers(&args.workers));

    let listener = tokio::net::TcpListener::bind(addr).await?;
    serve(listener, app.into_make_service()).await?;

    Ok(())
}
