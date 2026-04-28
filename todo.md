## token length aware routing mode
need tokenize request body to get token length
Then route to wokers based on token length configuration
dynamic configurable token length threshold

## kv cache hit rate aware routing mode
need merge request body to string and tokenize, then hash and query mooncake to get kv cache hit rate
Then route to wokers based on kv cache hit rate configuration

dynamic configurable kv cache hit rate threshold

兼容openai chat api，responses api，流式，非流式请求

optimize 镜像流量模式: 当前请求分发是串行执行的，而不是异步并行的

add test codes

can we avoid request - json conversion?

worker health check
support MHA model
support pipeline parallelism
For DeepSeek V3.2, query both kv cache and indexer cache, currently only kv cache
support 3:1 sparse attention model

routing based on both cache hit rate and load balance

增加token/cache router每个worker (mean, p50, P90, P99等) TTFT, TPOT, 请求长度的统计，实时metric信息，用于判断PD/非PD的路由负载阈值

高并发极限压测，需满足5000W TPM压测
评测这个模块对TTFT和TPOT的影响
优化这个模块导致的延迟
