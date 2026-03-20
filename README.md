# LLM 请求镜像流量 Router 工具

这是一个使用 Rust 语言开发的 LLM 请求镜像流量 router 工具，支持两种运行模式：镜像模式和分流模式。

## 功能特性

- **镜像模式**：将输入请求全部分流到每个配置的目标 IP 和端口，只返回第一个目标的结果
- **分流模式**：支持 round_robin 和 random 两种负载均衡策略，将请求选择性分发到不同的目标
- **动态管理目标**：支持通过 API 动态添加和删除目标 IP 和端口
- **OpenAI 兼容**：支持 OpenAI 风格的 `/v1/chat/completions` API

## 技术栈

- Rust 语言
- Axum 框架（HTTP 服务器）
- Reqwest 库（HTTP 客户端）
- Tokio 异步运行时
- Clap 命令行参数解析
- Serde 序列化/反序列化

## 编译步骤

1. 确保安装了 Rust 环境（推荐使用 rustup）
2. 进入项目目录
3. 运行编译命令：

```bash
cd llm_router
cargo build --release
```

编译完成后，可执行文件将位于 `target/release/llm_router`。

## 使用方式

### 基本用法

```bash
# 启动镜像模式，将请求分流到多个目标
./target/release/llm_router --port 8080 --targets "127.0.0.1:8000,127.0.0.1:8001" --mode mirror

# 启动分流模式，使用 round robin 策略
./target/release/llm_router --port 8080 --targets "127.0.0.1:8000,127.0.0.1:8001" --mode split --strategy round_robin

# 启动分流模式，使用 random 策略
./target/release/llm_router --port 8080 --targets "127.0.0.1:8000,127.0.0.1:8001" --mode split --strategy random
```

### 命令行参数

| 参数 | 描述 | 默认值 |
|------|------|--------|
| `--port` | 输入请求的端口 | 8080 |
| `--targets` | 输出请求的 IP 和端口，格式为 ip:port，多个用逗号分隔 | 空 |
| `--mode` | 运行模式：mirror（镜像模式）或 split（分流模式） | mirror |
| `--strategy` | 分流模式下的负载均衡策略：round_robin 或 random | round_robin |

### 管理目标 API

- **添加目标**：
  ```bash
  curl -X POST http://localhost:8080/api/targets \
    -H "Content-Type: application/json" \
    -d '{"ip": "127.0.0.1", "port": 8002}'
  ```

- **删除目标**：
  ```bash
  curl -X DELETE http://localhost:8080/api/targets \
    -H "Content-Type: application/json" \
    -d '{"ip": "127.0.0.1", "port": 8002}'
  ```

- **查看目标**：
  ```bash
  curl http://localhost:8080/api/targets
  ```

### 测试请求

```bash
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "gpt-3.5-turbo",
    "messages": [
      {"role": "user", "content": "Hello, how are you?"}
    ],
    "temperature": 0.7
  }'
```

## 运行示例

### 示例 1：镜像模式

```bash
./target/release/llm_router --port 8080 --targets "127.0.0.1:8000,127.0.0.1:8001" --mode mirror
```

此命令将启动一个运行在 8080 端口的 router，将所有请求镜像到 127.0.0.1:8000 和 127.0.0.1:8001，并返回第一个目标的响应。

### 示例 2：分流模式（Round Robin）

```bash
./target/release/llm_router --port 8080 --targets "127.0.0.1:8000,127.0.0.1:8001" --mode split --strategy round_robin
```

此命令将启动一个运行在 8080 端口的 router，使用轮询策略将请求分发到 127.0.0.1:8000 和 127.0.0.1:8001。

### 示例 3：分流模式（Random）

```bash
./target/release/llm_router --port 8080 --targets "127.0.0.1:8000,127.0.0.1:8001,127.0.0.1:8002" --mode split --strategy random
```

此命令将启动一个运行在 8080 端口的 router，使用随机策略将请求分发到三个目标。

## 注意事项

1. 确保目标服务器已经启动并运行在指定的端口上
2. 目标服务器需要支持与输入请求相同的 API 路径和格式
3. 在生产环境中，建议使用 `--release` 模式编译以获得最佳性能
4. 对于高并发场景，可能需要调整 Tokio 运行时的配置

## 故障排查

- **无法连接到目标服务器**：检查目标服务器是否运行，以及网络连接是否正常
- **请求超时**：检查目标服务器的响应时间，可能需要调整超时设置
- **响应格式错误**：确保目标服务器返回的响应格式与 OpenAI API 兼容

## 许可证

MIT
