# Mooncake HTTP Client

A command-line HTTP client for querying key existence in [Mooncake Store](https://github.com/kvcache-ai/Mooncake).

## Features

- ✅ Check if a single key exists
- ✅ Batch check multiple keys
- ✅ List all keys from the store
- ✅ List all segments
- ✅ Health checks
- ✅ Prometheus metrics retrieval
- ✅ Async/await support with Tokio

## Prerequisites

- Rust 1.70+
- A running Mooncake Master service with HTTP enabled

## Installation

Build from source:

```bash
cd mooncake-http-client
cargo build --release
```

The binary will be available at `target/release/mooncake-http-client`.

## Usage

```
mooncake-http-client --url <URL> <COMMAND>
```

### Commands

| Command | Description |
|---------|-------------|
| `check --key <KEY>` | Check if a single key exists |
| `check-batch --keys <KEYS>` | Check multiple keys (comma-separated) |
| `list-keys` | List all keys in the store |
| `list-segments` | List all segments |
| `health` | Check server health |
| `metrics` | Get Prometheus metrics |

### Options

| Option | Description | Default |
|--------|-------------|---------|
| `-u, --url <URL>` | Mooncake server URL | `http://localhost:metrics_port` |
| `-h, --help` | Print help | - |
| `-V, --version` | Print version | - |

### Examples

#### Check a single key

```bash
mooncake-http-client --url http://mooncake_master_ip:metrics_port check --key my_key
```

Output:
```
Connecting to Mooncake Store at: http://mooncake_master_ip:metrics_port

=== Single Key Query ===
Checking if key 'my_key' exists...
Key exists: true
```

#### Check multiple keys

```bash
mooncake-http-client --url http://mooncake_master_ip:metrics_port check-batch --keys "key1,key2,key3"
```

Output:
```
Connecting to Mooncake Store at: http://mooncake_master_ip:metrics_port

=== Batch Key Query ===
Checking keys: ["key1", "key2", "key3"]
  key1: EXISTS
  key2: NOT FOUND
  key3: EXISTS
```

#### List all keys

```bash
mooncake-http-client --url http://mooncake_master_ip:metrics_port list-keys
```

#### List all segments

```bash
mooncake-http-client --url http://mooncake_master_ip:metrics_port list-segments
```

#### Health check

```bash
mooncake-http-client --url http://mooncake_master_ip:metrics_port health
```

Output:
```
Connecting to Mooncake Store at: http://mooncake_master_ip:metrics_port

=== Health Check ===
Healthy: true
Message: OK
```

#### Get metrics

```bash
mooncake-http-client --url http://mooncake_master_ip:metrics_port metrics
```

### With logging

Enable debug logging with `RUST_LOG`:

```bash
RUST_LOG=info mooncake-http-client --url http://mooncake_master_ip:metrics_port health
```

## Mooncake Master Configuration

Ensure your Mooncake Master is running with HTTP metrics enabled:

```bash
mooncake_master \
  --eviction_high_watermark_ratio=0.95 \
  --rpc_address mooncake_master_ip \
  --rpc_port 50061 \
  --enable_http_metadata_server=true \
  --http_metadata_server_host mooncake_master_ip \
  --http_metadata_server_port 8090 \
  --enable_metric_report=true \
  --metrics_port metrics_port
```

The HTTP client uses the `metrics_port` (e.g., metrics_port) for queries.

## Development

Build:
```bash
cargo build
```

Run with logging:
```bash
RUST_LOG=debug cargo run -- --url http://localhost:metrics_port health
```

## License

This project is licensed under the MIT OR Apache-2.0 license.
