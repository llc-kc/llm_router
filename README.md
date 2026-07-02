# LLM KVCache感知的异构部署路由 LLM KVCache-Aware Heterogeneous Routing (LLM_CAHR)

这是一个使用 Rust 语言开发的 LLM 请求镜像流量 router 工具，支持3种运行模式：

- 镜像模式 (mirror)
- (round_robin/random)分流模式 (split)
- 缓存感知模式 (cache_route)


## 功能特性

- **镜像模式 (mirror)**：将输入请求全部分流到每个配置的目标 IP 和端口，只返回第一个目标的结果
- **分流模式 (split)**：支持 round_robin 和 random 两种负载均衡策略，将请求选择性分发到不同的目标
- **缓存路由模式 (cache_route)**：基于KV cache命中率，将请求路由到合适的worker
- **动态管理目标**：支持通过 API 动态添加和删除目标
- **OpenAI 兼容**：支持 OpenAI 风格的 `/v1/chat/completions` API


## 缓存路由模式 (cache_route) 工作原理

缓存路由模式根据请求的KV cache命中率来智能路由请求：

1. **Tokenize**：将请求文本通过tokenizer转换为token IDs
2. **分组**：按照`page_size`（默认64）将token IDs分组
3. **计算Hash**：使用SHA256计算每组的哈希值
4. **查询Mooncake**：通过Mooncake HTTP Client批量查询哪些hash存在于KV cache中
5. **计算命中率**：`hit_rate = (命中页数 × page_size) / 总token数`
6. **路由选择**：根据命中率选择满足阈值的最优worker

这种模式适用于异构GPU集群，可以将高缓存命中率的请求路由到具有相应缓存的worker，提高推理效率。

## 编译步骤

1. 确保安装了 Rust 环境（推荐使用 rustup）
2. 进入项目目录
3. 运行编译命令：

```bash
cd llm_cahr
cargo build --release
```

编译完成后，可执行文件将位于 `target/release/llm_cahr`。


## 使用方式

### 基本用法

```bash
# 启动镜像模式，将请求分流到多个目标
./target/release/llm_cahr --port 8080 --workers '[{"url":"http://127.0.0.1:8000"},{"url":"http://127.0.0.1:8001"}]' --mode mirror

# 启动分流模式，使用 round robin 策略
./target/release/llm_cahr --port 8080 --workers '[{"url":"http://127.0.0.1:8000"},{"url":"http://127.0.0.1:8001"}]' --mode split --strategy round_robin

# 启动分流模式，使用 random 策略
./target/release/llm_cahr --port 8080 --workers '[{"url":"http://127.0.0.1:8000"},{"url":"http://127.0.0.1:8001"},{"url":"http://127.0.0.1:8002"}]' --mode split --strategy random

# 启动缓存路由模式
./target/release/llm_cahr --port 8080 --workers '[{"url":"http://127.0.0.1:8000","cache_hit_rate":0.8},{"url":"http://127.0.0.1:8001","cache_hit_rate":0.5}]' --mode cache_route --tokenizer-path /path/to/tokenizer --mooncake-url http://mooncake_master_ip:metrics_port --page-size 64
```

### 命令行参数

| 参数 | 描述 | 默认值 |
|------|------|--------|
| `--port` | 输入请求的端口 | 8080 |
| `--workers` | Worker配置，JSON格式数组字符串 | 空 |
| `--mode` | 运行模式：mirror、split 或 cache_route | mirror |
| `--strategy` | 分流模式下的负载均衡策略：round_robin 或 random | round_robin |
| `--tokenizer-path` | Tokenizer模型文件路径，用于cache_route模式 | 空 |
| `--mooncake-url` | Mooncake服务器URL，用于cache_route模式 | 空 |
| `--page-size` | Token分组大小，用于cache_route模式 | 64 |
| `--use-eagle` | 只针对cache路由模式。启用eagle模式，使用bigram处理token并将page_size乘以2 | 无 |
| `--add-prefix-tokens` | 只针对cache路由模式。在计算缓存命中率时添加到token前面的前缀token，逗号分割的整数向量 | 空 |
| `--log-level` | 日志级别：error、warn、info、debug、trace。也可通过环境变量 `RUST_LOG` 设置 | info |

### Worker配置格式

`--workers` 参数接受 JSON 格式的数组字符串，每个 worker 对象包含以下字段：

- `url` (必需): Worker的URL，格式为 `http://ip:port`
- `cache_hit_rate` (可选): 缓存命中率，用于缓存感知模式

示例：
```json
[
  {"url": "http://127.0.0.1:8000"},
  {"url": "http://127.0.0.1:8001", "cache_hit_rate": 0.8}
]
```

**不同模式的字段说明：**

- **mirror/split模式**：只需要 `url` 字段
- **cache_route模式**：需要 `url` 和 `cache_hit_rate` 字段，`cache_hit_rate` 表示该worker所需的最小缓存命中率（0.0-1.0）

### 管理目标 API

- **添加目标**：
  ```bash
  curl -X POST http://localhost:8080/api/workers \
    -H "Content-Type: application/json" \
    -d '{"url": "http://127.0.0.1:8002"}'
  ```

- **删除目标**：
  ```bash
  curl -X DELETE http://localhost:8080/api/workers \
    -H "Content-Type: application/json" \
    -d '{"url": "http://127.0.0.1:8002"}'
  ```

- **查看目标**：
  ```bash
  curl http://localhost:8080/api/workers
  ```

### 测试请求

```bash
curl -s http://localhost:30000/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{"model": "DeepSeek-V3.2", "messages": [{"role": "user", "content": "Hello, how are you?"}]}'
```

## 运行示例

### 示例 1：镜像模式

```bash
./target/release/llm_cahr --port 8080 --workers '[{"url":"http://127.0.0.1:8000"},{"url":"http://127.0.0.1:8001"}]' --mode mirror
```

此命令将启动一个运行在 8080 端口的 router，将所有请求镜像到 127.0.0.1:8000 和 127.0.0.1:8001，并返回第一个目标的响应。

### 示例 2：分流模式（Round Robin）

```bash
./target/release/llm_cahr --port 8080 --workers '[{"url":"http://127.0.0.1:8000"},{"url":"http://127.0.0.1:8001"}]' --mode split --strategy round_robin
```

此命令将启动一个运行在 8080 端口的 router，使用轮询策略将请求分发到 127.0.0.1:8000 和 127.0.0.1:8001。

### 示例 3：分流模式（Random）

```bash
./target/release/llm_cahr --port 8080 --workers '[{"url":"http://127.0.0.1:8000"},{"url":"http://127.0.0.1:8001"},{"url":"http://127.0.0.1:8002"}]' --mode split --strategy random
```

此命令将启动一个运行在 8080 端口的 router，使用随机策略将请求分发到三个目标。

### 示例 4：缓存路由模式

```bash
./target/release/llm_cahr --port 8080 --workers '[{"url":"http://127.0.0.1:8000","cache_hit_rate":0.8},{"url":"http://127.0.0.1:8001","cache_hit_rate":0.5}]' --mode cache_route --tokenizer-path /path/to/tokenizer --mooncake-url http://mooncake_master_ip:metrics_port --page-size 64
```

此命令将启动一个运行在 8080 端口的 router，根据请求的KV cache命中率将请求路由到合适的worker：
- cache命中率 >= 0.8 的请求路由到 127.0.0.1:8000
- cache命中率 >= 0.5 的请求路由到 127.0.0.1:8001
- cache命中率 < 0.5 的请求路由到最后一个worker (127.0.0.1:8001)

**使用Eagle模式**：
```bash
./target/release/llm_cahr --port 8080 --workers '[{"url":"http://127.0.0.1:8000","cache_hit_rate":0.8},{"url":"http://127.0.0.1:8001","cache_hit_rate":0.5}]' --mode cache_route --tokenizer-path /path/to/tokenizer --mooncake-url http://mooncake_master_ip:metrics_port --page-size 64 --use-eagle
```

启用 `--use-eagle` 后：
- token会被转换为bigram格式（相邻token对）后再计算hash
- page_size会自动乘以2（例如64变为128）

**添加前缀Token**：
```bash
./target/release/llm_cahr --port 8080 --workers '[{"url":"http://127.0.0.1:8000","cache_hit_rate":0.8},{"url":"http://127.0.0.1:8001","cache_hit_rate":0.5}]' --mode cache_route --tokenizer-path /path/to/tokenizer --mooncake-url http://mooncake_master_ip:metrics_port --page-size 64 --add-prefix-tokens "0,1,2,3"
```

启用 `--add-prefix-tokens` 后：
- 在计算缓存命中率时，会将指定的前缀token添加到输入token的前面，以保证prompt token id与sglang引擎完全对齐，从而得到正确的cache hash
- 例如 `--add-prefix-tokens "1,2,3,4"` 会在输入token前添加 `[1, 2, 3, 4]`
- 例如DeepSeek需要加一个0的token前缀，因为tokenizer拼接请求时没有加这个特殊token。

**注意**：cache_route模式需要：
1. 指定 `--tokenizer-path` 参数
2. 指定 `--mooncake-url` 参数（Mooncake服务器地址）
3. workers配置中包含 `cache_hit_rate` 字段

### 日志级别配置

可以通过 `--log-level` 参数或 `RUST_LOG` 环境变量设置日志级别：

```bash
# 使用命令行参数设置日志级别
./target/release/llm_cahr --port 8080 --mode mirror --log-level debug

# 使用环境变量设置日志级别
RUST_LOG=debug ./target/release/llm_cahr --port 8080 --mode mirror
```

支持的日志级别（从低到高）：
- `trace` - 最详细的日志，包含所有调试信息
- `debug` - 调试信息，包含详细的处理过程
- `info` - 一般信息，默认级别，包含启动信息和关键处理步骤
- `warn` - 警告信息
- `error` - 错误信息

## 注意事项

1. 确保目标服务器已经启动并运行在指定的端口上
2. 目标服务器需要支持与输入请求相同的 API 路径和格式
3. 在生产环境中，建议使用 `--release` 模式编译以获得最佳性能
4. 对于高并发场景，可能需要调整 Tokio 运行时的配置
5. cache_route模式需要指定 `--tokenizer-path` 和 `--mooncake-url` 参数，且workers配置中需要包含 `cache_hit_rate` 字段

## 故障排查

- **无法连接到目标服务器**：检查目标服务器是否运行，以及网络连接是否正常
- **请求超时**：检查目标服务器的响应时间，可能需要调整超时设置
- **响应格式错误**：确保目标服务器返回的响应格式与 OpenAI API 兼容
- **cache_route模式启动失败**：检查 `--tokenizer-path` 和 `--mooncake-url` 参数是否正确指定，以及workers配置中是否包含 `cache_hit_rate` 字段
- **cache_route模式查询失败**：检查Mooncake服务器是否正常运行，以及网络连接是否正常
