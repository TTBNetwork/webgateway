# Context Summary — 2026-11-20T05:40:40Z

## 当前状态
- 分支 `feat/rate-limit`（叠加在 `feat/observability` 748f6bf 之后），HEAD ad4196f。
- 已 push 到 origin，CI 跟踪 ad4196f。
- schema 仍为 **v6**（本轮无迁移）；E2E 数据已清理（users/websites/gateways/logs/stats = 0），
  测试进程与端口已释放。

## 本轮完成（每 IP 限流 U-07 部分 / F-57）

### 实现
- 新增 `gateway/src/ratelimit.rs`：令牌桶，16 个分片 `Mutex<HashMap<IpAddr, Bucket>>` +
  全局 `AtomicUsize` tracked 计数。
- 配置 `[server].rate_limit`：`enabled`、`requests_per_second`、`burst`（0 => ceil(rps)）、
  `max_tracked_ips`（下限 1024）；环境变量 `GATEWAY_RATE_LIMIT_RPS`（>0 自动启用）、
  `_BURST`、`_MAX_TRACKED`；非法值启动失败。
- 客户端 IP 用站点 `client_ip` 解析（raw / proxyProtocol / xForwardedFor / xRealIP），
  与访问日志 `remote_addr` 一致；检查在站点命中后、缓存/上游之前。
- 429 响应：`Retry-After: <秒>` + 访问日志（status=429）；HTTP/1.1、HTTP/2、HTTP/3、
  WebSocket、CONNECT 全部接入（h3 走 `buffered_rate_limit_response`，body 为 Bytes）。
- 表满策略：先 prune >60s 未访问条目，仍满则 fail-open 放行（避免误伤全部流量）。
- 指标新增 `webgateway_rate_limited_total`、`webgateway_rate_limit_tracked_ips`。
- 共享 helper：`proxy::rate_limit_decision`（判定+日志）、`rate_limit_response`、
  `buffered_rate_limit_response`；`buffered_error` 拆出 `buffered_text_response`。

### 实测（GATEWAY_RATE_LIMIT_RPS=1，BURST=2，beta DB）
- 同一 IP 连续 4 次：200 / 200 / 429 / 429；429 响应带 `retry-after: 1`。
- 换源 IP（`curl --interface 127.0.0.2`）→ 200，说明桶按 IP 隔离。
- 指标：`rate_limited_total 3`、`rate_limit_tracked_ips 2`、`requests_total 6`。
- 访问日志出现 status=429 行（200 行同时存在）。

### 文档
- `gateway.md`：配置块、环境变量表、2.3 限流行为/指标/单节点限制。
- `deployment.md`：gateway 环境变量、限制表（改为无 WAF/CC 规则）、运维说明。
- `gateway/config.example.toml` 增加 `[server.rate_limit]` 示例。
- `issue.md` U-07（部分）/F-57/优先级；`changelog.md` 新小节。

## 测试
- gateway 45（+6 ratelimit 单测）、dashboard 25、shared 18 + 3 集成；
  `cargo fmt` / `clippy -D warnings` 通过。
- CI 使用 `--locked`；本轮未新增依赖。

## 关键事实 / gotchas
- 限流是**单节点进程内**的：多节点部署时各自计数；限流配置不支持 NOTIFY 热更新（需重启）。
- 限流键使用站点 client_ip 配置；若站点信任 XFF，伪造头会影响分桶（与日志口径一致）。
- WebSocket/CONNECT 在站点命中后检查；未命中站点直接 404/403，不参与限流。
- h3 的 429 响应是完整缓冲 body（与其他 h3 错误一致）。
- 表满 fail-open，不会因为跟踪表满而拒绝全部客户；指标里 tracked_ips 可观测。
- 测试用 gateway 端口 18085 / 指标 19101 / 后端 18080。

## 下一步（生产清单剩余）
1. 上游健康检查/失败摘除（P-10/U-17）：建议 BackendHealth registry + 主动探测 + 被动标记，
   选择时优先健康后端、优先级组回退；指标 `backend_up`/检查计数；E2E 死后端自动跳过。
2. HTTP/3 流式、缓存接入、停机 drain（B-04/P-01）。
3. 缓存正确性：Vary、single-flight、HEAD/304（B-09）。
4. 统计 total_ips/保留期/UTC 日（B-08/B-14）。
5. refresh token / 每用户 API Key / 服务令牌过期（B-30 剩余）。
6. B-21 分支整合（需用户确认）；CI DB E2E（U-31）。
