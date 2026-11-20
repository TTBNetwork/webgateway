# Context Summary — 2026-11-20T04-14-59Z

## 当前状态
- 分支 `feat/limits-hardening`（叠加在 `feat/reliability-hardening` 之后）。
- schema 仍为 **v6**（本轮无迁移）；资源上限测试数据已清理
  （users/websites/certificates/gateways/logs/stats = 0），测试进程已停止。

## 本轮完成（B-04 部分 / B-09 部分 / B-11）

### 配置
- `gateway/config.example.toml` 新增：
  - `max_connections`（默认 4096）
  - `max_header_bytes`（默认 32 KiB，最小 8 KiB）
  - `max_request_body_bytes`（默认 64 MiB）
  - `max_response_body_bytes`（默认 64 MiB）
- 环境变量：`GATEWAY_MAX_CONNECTIONS` / `GATEWAY_MAX_HEADER_BYTES` /
  `GATEWAY_MAX_REQUEST_BODY` / `GATEWAY_MAX_RESPONSE_BODY`；Config::load 做下限 clamp。
- `AppState` 保存 body 上限；`Listeners::new(bind, max_connections, max_header_bytes)`。

### 请求/响应/连接限制
- 上游 client 请求体类型从 `Incoming` 改为 boxed `GatewayBody`，以便用 `http_body_util::Limited` 包装。
- 请求体：Content-Length > 上限 → 转发前 **413**；chunked/无长度 → `Limited`，达到上限后上游请求失败。
- 响应：缓存只在 Content-Length 可解析且 ≤ `min(cache.max_entry_size, max_response_body_bytes)`
  时 collect；chunked 直接流式转发、不缓存（修复 B-09 的“先 collect 再判断”）。
- h3：声明或累计请求体超限 → **413**；`handle_buffered` 响应超限 → **502**。
- header：HTTP/1 `max_buf_size` + HTTP/2 `max_header_list_size`；超限由 hyper 处理（实测 431）。
- 连接：TCP 监听共享 `Semaphore`；超限新连接直接关闭并 WARN；permit 随连接任务释放。
- 优雅停机/access log 逻辑保持不变。

### 实测（max_connections=1, max_header=8192, max_request_body=1024）
- 2 KB body → 413 Payload Too Large；512 B body 正常到后端（后端 POST 501 属测试后端限制）。
- 20 KB header → 431。
- 并发两个请求：第二个 curl exit 56，第一个 3s 慢请求 200。
- 开缓存站点：首次 MISS 3.0s，第二次 HIT 0.001s（x-webgateway-cache）。
- 网关日志：`request body exceeds the configured limit`、`connection limit reached`。

## 关键事实
- `UpstreamClient` 现在是 `Client<UpstreamConnector, GatewayBody>`；`BufferedUpstreamClient` 仍为 `Full<Bytes>`。
- 缓存现在要求 Content-Length；无长度/chunked 响应不再缓存（行为变化，已文档化）。
- HTTP/3 仍不接入缓存；h3 在途请求不参与停机 drain；h3 header/connection 限制未单独做。
- 连接上限是 TCP 全局（所有监听端口共享），HTTP/3 连接不计入。
- 站点级覆盖、连接/请求超时仍未实现。

## 测试
- gateway 36、dashboard 10、shared 14 + 3 集成；fmt/clippy 通过。
- E2E 使用 `.tools/tmp/slow_server.py`、`limit_site.sql`；已清理站点/日志/网关行。

## 下一步
1. B-04 剩余：h3 缓存接入/流式、h3 drain；站点级 body/connection 限制与超时。
2. B-07 CONNECT 失败日志/端口白名单；B-12 Alt-Svc；B-15 CSP/localStorage。
3. refresh token / 每用户 API Key / 服务令牌轮换；B-21 分支合并。
4. 证书：ZeroSSL EAB 实测、通配符、私钥加密。
