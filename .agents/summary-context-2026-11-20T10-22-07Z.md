# Context Summary — 2026-11-20T10:22:07Z

## 当前状态
- 分支 `feat/cache-vary`（叠加在 `feat/h3-streaming` 30ac6b0 之后），HEAD 61d6c0b。
- 分支栈：feat/health-checks (a5aee76) → feat/h3-streaming (30ac6b0) → feat/cache-vary (61d6c0b)。
- h3-streaming CI run 44718152074 **success**；cache-vary CI 进行中（见最终回复）。
- schema 仍为 **v6**（本轮无迁移）；E2E 数据已清理（users/websites/certificates/gateways/logs/stats = 0），
  测试进程与端口已释放。

## 本轮完成（两个批次）

### 1) HTTP/3 流式响应 + 停机 drain（F-59 / B-04）
- `handle_buffered` 返回 `Response<GatewayBody>`：h3 响应从上游逐帧 `send_data`，不再整体 collect；
  `max_response_body_bytes` 不再限制 h3 响应。
- h3 请求改用 `state.client`（boxed Full request body），请求体仍受 `max_request_body_bytes`（超限 413）。
- 新增 `http3::Http3` 句柄（endpoints + accept tasks）：SIGTERM 时 `stop_accepting`（只 abort accept loop，
  不断开在途流）→ h3 请求与 TCP 共享 `listeners::Inflight` 计数 → 10s drain → `endpoint.close()`。
- E2E（aioquic）：
  - 64 KiB h3 响应（GATEWAY_MAX_RESPONSE_BODY=1024）→ 200 + 完整 65536 字节；
  - 3s 慢 h3 请求在 1s 后 SIGTERM：客户端 200/7 字节，gateway exit 0，日志显示 drain 后 offline。

### 2) 缓存 Vary / HEAD / 304（F-60 / B-09）
- `cache.rs` 重构：条目存 `Vary` 请求头值，按变体查找；同 key 多 variant；LRU 按 (key, token)；
  `Vary: *` 拒绝缓存；统计/淘汰/占用按 variant。
- `proxy.rs`：GET/HEAD 都查缓存，仅 GET 写入；HEAD 从 GET 缓存返回（保留 Content-Length、无 body）；
  GET/HEAD 的 `If-None-Match` 返回 304；`Vary` 响应不再整体拒绝。
- E2E：gzip/br/无编码三个变体分别 MISS 并缓存各自 body，gzip 重复 HIT；HEAD 命中
  HIT + Content-Length=8、无 body；GET/HEAD + If-None-Match 均 304；指标 hits 5/misses 4/stores 4/entries 4。

## 测试
- gateway 53（+2 cache；h3/代理/限流/健康检查单测全部通过）、dashboard 25、shared 18 + 3 集成；
  fmt/clippy -D warnings 通过。CI 使用 `--locked`，两批都未新增依赖。

## 关键事实 / gotchas
- h3 请求体仍整体缓冲；h3 不走缓存；WS/gRPC over h3、0-RTT/连接迁移未实现。
- h3 E2E 使用 `.tools/venv` 的 aioquic 1.8：SNI 必须放在 `QuicConfiguration(..., server_name=...)`，
  `connect()` 不接受 server_name；证书 fixture 用 openssl 自签 + 面板 API 上传。
- 缓存 key 仍为 host|path_and_query，不含方法；HEAD 复用 GET 变体（GET/HEAD 共享 key 是有意行为）。
- `Vary: *`、Set-Cookie、no-store/private/no-cache 仍禁止缓存；`If-Modified-Since` 未实现。
- **重要**：本轮曾在 feat/health-checks 上误用 `git reset --hard`，短暂丢掉 AGENT.md 的未提交用户编辑；
  已按原文恢复（两条 .secrets 说明）。以后不要对工作区 `reset --hard`，需要移动提交时先建分支或 stash。
- 分支移动方式：先在当前 commit 建新分支，再回到旧分支 reset 到远端 commit（本项目可接受，但务必确认无用户改动）。

## 下一步（剩余生产清单）
1. 统计准确性：total_ips 跨秒、保留期配置、UTC 日（B-08/B-14）。
2. refresh token / 每用户 API Key / 服务令牌过期（B-30 剩余）。
3. 缓存 single-flight、If-Modified-Since、持久化；h3 缓存与请求体流式。
4. WAF/CC、跨节点健康状态/熔断、metrics 告警后端（U-07/P-10/U-19 剩余）。
5. B-21 分支整合（需用户确认）；CI DB E2E（U-31）依赖 PG18+uint128 镜像。
