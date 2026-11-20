# WebGateway v2 — Features & Issue Tracker

> 生成时间：2026-11-19；更新：2026-11-20  
> 截至提交：feat/connect-alt-svc f6fe9a4 + 面板会话安全改造（feat/dashboard-session-security）  
> 验证环境：beta PostgreSQL（schema v6）、本地 E2E、GitHub Actions  
> 图例：✅ 已完成并验证；⚠️ 可用但有明确限制；❌ 未实现

---

## 1. 已实现 Features

### 1.1 平台 / 数据库 / 主控

| ID | Feature | 状态 | 说明 |
|---|---|---|---|
| F-01 | Rust workspace | ✅ | crates/shared、dashboard/backend、gateway；edition 2024 |
| F-02 | CI 流水线 | ✅ | fmt、clippy -D warnings、workspace 单测、前端 lint/build；CI run 44545263511 全绿 |
| F-03 | PostgreSQL schema v3 | ✅ | 迁移 0001/0002/0003，schema_state 版本标记，_sqlx_migrations |
| F-04 | 数据库表结构 | ✅ | users/configs/certificates/DNS/websites/gateways、4 张访问日志表、秒/分/日统计表 |
| F-05 | 周分区 | ✅ | access_* 四张表按周分区；迁移预建，主控启动/每小时续建当前周 + 3 周，整周过期分区 DROP |
| F-06 | NOTIFY 触发器 | ✅ | websites/certificates/configurations/dns_providers 的 INSERT/UPDATE/DELETE |
| F-07 | 主控独占迁移 | ✅ | advisory lock；网关只等待 schema_state，不执行 DDL |
| F-08 | 主控 /health | ✅ | 返回数据库状态与 schema 版本 |
| F-09 | 统计 rollup | ✅ | 主控 10s 任务；水位 `stats_state.last_aggregated_at` + 90s 重算；缺口按分钟/秒回填；秒→分→日；事务级 advisory lock |
| F-10 | 网关生命周期 | ✅ | 等待主控、注册 online、10s 心跳、SIGTERM/Ctrl-C offline |
| F-11 | 动态端口 | ✅ | 配置端口 ∪ websites.ports，同步后自动增删监听 |
| F-12 | NOTIFY + 水位同步 | ✅ | LISTEN websites/certificates；重连后比较 (COUNT, MAX(updated_at)) |

### 1.2 协议 / TLS

| ID | Feature | 状态 | 说明 |
|---|---|---|---|
| F-13 | HTTP/1.1 代理 | ✅ | 200/404/502/504、60s 超时 |
| F-14 | HTTPS + HTTP/2 入站 | ✅ | rustls，ALPN h2/http/1.1，E2E 协商 h2 |
| F-15 | TLS SNI | ✅ | 精确 → 通配符 → 默认自签证书（rcgen） |
| F-16 | HTTP/3 入站 | ✅ | quinn/h3/h3-quinn，UDP 监听，TLS 1.3 + ALPN h3 |
| F-17 | HTTP/3 同连接并发 | ✅ | 每个请求独立 spawn；aioquic 单连接 5 请求全部 200 |
| F-18 | 上游 HTTP / HTTPS | ✅ | hyper-rustls HttpsConnector；可选 ca_file；webpki 根 |
| F-19 | gRPC 上游 | ✅ | h2c/TLS、application/grpc 检测、TE: trailers、无 60s 超时；Nezha v2.6.2 + agent v2.4.0 真实 E2E 通过（一元/双向流/trailers） |
| F-20 | WebSocket 升级 | ✅ | h1 Upgrade、101、双向桥接；wss（TLS）+ 后端 base path；MCSManager daemon 4.19.3 的明文 / wss / 带前缀三种场景实测通过 |
| F-21 | CONNECT 隧道 | ⚠️ | h1；authority host 必须命中已配置站点；非 MITM |
| F-22 | PROXY protocol v1/v2 | ✅ | 连接入口解析；站点级 get_request_ip 决定信任 |
| F-23 | 客户端 IP 模式 | ✅ | raw / proxyProtocol / xForwardedFor / xRealIP；E2E 验证 |
| F-24 | 访问日志 | ✅ | 请求/响应/秒级 size 四表；500ms 或 256 条批量；DB now() |
| F-25 | 统计聚合 | ✅ | 主控从日志聚合秒/分/日；E2E 通过 |
| F-26 | HTTP/3 访问日志 | ✅ | http_version=HTTP/3.0，请求/响应字节 |

### 1.3 路由 / 负载均衡 / 请求头

| ID | Feature | 状态 | 说明 |
|---|---|---|---|
| F-27 | Host 路由 | ✅ | 精确 + *.example.com（单 label，最长后缀） |
| F-28 | 多条件路由 | ✅ | match_path + match.path/UA/query/method，组合 AND |
| F-29 | 权重 / 优先级 | ✅ | weight 默认 1；priority 越小越优先；备用组静态 |
| F-30 | 站点 headers | ✅ | set/remove、$remote_addr/$scheme/$host、逐跳头剔除 |
| F-31 | 节点标识头 | ✅ | X-WebGateway-Node: cn-shanghai-<16 hex> |
| F-32 | 缓存 | ⚠️ | 默认 16MiB LRU（可配）、TTL/类型白名单、ETag/304、HIT/MISS/Age；h3 不缓存 |

### 1.4 管理面板

| ID | Feature | 状态 | 说明 |
|---|---|---|---|
| F-33 | 面板 API | ✅ | 读接口 + 证书/站点/网关/用户写接口；错误/校验统一 JSON |
| F-34 | API 鉴权 | ✅ | TOTP + 每用户 JWT（`wg_session` HttpOnly/SameSite=Strict Cookie，响应不返回 token）；admin/user/view RBAC；token_version 撤销（logout/revoke/角色变更/重置 TOTP）；服务令牌恒时比较、多值轮换、视为 admin；无 refresh token / 每用户 API key |
| F-35 | Vue 3 面板 | ✅ | 登录、概览、站点 CRUD、证书 CRUD、网关、日志、用户管理、设置 |
| F-36 | 视觉 / 交互 | ✅ | 深/浅色、中/英、响应式侧栏、统计卡片、SVG 流量图 |
| F-37 | 前端工程 | ✅ | 自写 history router + 原生 fetch；无新增依赖；package/lock 未改 |
| F-38 | 面板验证 | ✅ | Vite 代理 401/200 + 真实数据；format/lint/build 通过 |
| F-53 | 面板会话与响应安全 | ✅ | HttpOnly Cookie 会话、安全响应头（CSP/HSTS/nosniff/DENY/no-store）、内部错误脱敏 + error_id、服务令牌恒时比较多值轮换 |
| F-54 | 生产部署基础 | ✅ | dashboard 静态托管 + SPA 回退（`DASHBOARD_STATIC_DIR`）、路径穿越防护与资源缓存策略、SIGTERM/Ctrl-C 优雅停机（30s）、`deployment.md` 部署指南 |
| F-55 | 密钥静态加密 | ✅ | AES-256-GCM + `enc:v1:` 前缀；`SECRET_ENCRYPTION_KEY`/`_PREVIOUS`；覆盖 TOTP/JWT/客户端 secret、证书私钥、ACME account key、DNS 凭据；启动幂等加密存量明文；gateway 读取时解密 |
| F-56 | Prometheus 指标 | ✅ | dashboard/gateway 独立回环 `/metrics`：请求、h3/WS/CONNECT、缓存命中/eviction/占用、连接/在途、访问日志丢弃与队列、配置同步、证书/路由数、统计水位、登录/API 错误；deployment.md 含抓取与告警示例 |
| F-57 | 每 IP 限流 | ✅ | 令牌桶（rps + burst，16 分片、全局 IP 上限、过期清理、满表 fail-open）；按站点 client_ip 解析客户端 IP；HTTP/1/2/3/WS/CONNECT 统一；429 + Retry-After + 访问日志；指标 `rate_limited_total`/`rate_limit_tracked_ips` |
| F-58 | 上游健康检查 | ✅ | 站点级 `config.health_check`：主动 GET 探测（interval/timeout/阈值）+ 被动失败标记；选择时优先健康后端、全不健康回退完整集合、priority 备用组自动生效；指标 `health_check_*`、`backends_unhealthy` |

### 1.5 测试

| ID | Feature | 状态 | 说明 |
|---|---|---|---|
| F-39 | 网关单测 | ✅ | 36 个；fmt/clippy 全通过 |
| F-40 | shared 测试 | ✅ | 14 单测 + 3 集成（临时库，含 stats 回填/分区回收、schema v6） |
| F-41 | 本地 E2E | ✅ | NOTIFY、headers、PROXY、路由、日志、统计、缓存、TLS、WS、CONNECT、h3、权重、TOTP/JWT；Nezha gRPC 与 MCSManager WS 联调；证书上传与 Let's Encrypt 真实签发；优雅停机 drain + TOTP 重放/限速；body/header/connection 上限；CONNECT 200/403 日志 |

### 1.6 证书管理

| ID | Feature | 状态 | 说明 |
|---|---|---|---|
| F-42 | 证书手动上传 | ✅ | POST /api/certificates；SAN/私钥校验、到期时间解析；入库后网关 NOTIFY 加载 |
| F-43 | ACME 自动签发/续期 | ✅ | ZeroSSL（EAB 优先）→ Let's Encrypt；DNS-01 腾讯云 DNSPod；账户 key 持久化、10 分钟租约、到期前 30 天续期；实测 LE 签发成功 |
| F-44 | 证书面板 | ✅ | 自动/手动添加、重新签发、删除、状态/错误展示 |
| F-45 | 站点/网关 CRUD | ✅ | 站点创建/更新/删除（JSON 后端与配置校验）；网关名称/位置更新与删除 |
| F-46 | 用户管理 / RBAC / 撤销 | ✅ | 用户列表/创建/角色/删除/TOTP 重置/会话撤销；token_version JWT 撤销；admin/user/view 路由级 RBAC；面板用户页 |
| F-47 | TOTP 防重放 / 登录限速 | ✅ | users.last_totp_step 单调 step；用户/IP 失败计数，5 次/5 分钟锁 5 分钟；429 + retry_after |
| F-48 | 优雅停机 / 日志可靠性 | ✅ | SIGTERM 停止 accept → 10s drain in-flight（HTTP/TCP）→ flush access log → offline；批量写失败重试 3 次 + 丢弃计数 |
| F-49 | 请求/响应/连接上限 | ✅ | max_connections / max_header_bytes / max_request_body_bytes / max_response_body_bytes；413/431/丢弃；缓存与 h3 共用响应上限 |
| F-50 | 缓存收集前置检查 | ✅ | 仅 Content-Length 可解析且 ≤ 上限时缓存；chunked 不缓存；避免先 collect 再判断 |
| F-51 | CONNECT 审计与端口白名单 | ✅ | 400/403/502/504/200 全写访问日志；`connect_ports` 默认 [443,80]，可 env 覆盖 |
| F-52 | HTTP/3 Alt-Svc | ✅ | TLS h1/h2 响应带 `h3=":<udp_port>"; ma=86400`；h3/明文不加 |

---

## 2. 部分完成 / 有明确限制

| ID | Feature | 限制 |
|---|---|---|
| P-01 | HTTP/3 | 请求/响应全缓冲，无流式；无 0-RTT/连接迁移；h3 不走缓存；WS/gRPC over h3 未实现；已有 body/response 上限；TLS h1/h2 已广播 Alt-Svc |
| P-02 | gRPC | 已在 Nezha 上验证；访问日志只记 HTTP 200，无法区分 grpc-status；无 gRPC-Web |
| P-03 | WebSocket | 升级连接字节数记 0；无 mTLS / SNI 覆盖；wss 与后端 base path 已支持（2026-11-19） |
| P-04 | CONNECT | 仅 h1；已修复 400/403 日志与端口白名单；无鉴权 / mTLS；无 HTTP/2 extended CONNECT |
| P-05 | 缓存 | 仅 h1/h2；配置同步清空全部；无持久化/Vary/HEAD/stale-while-revalidate/single-flight/命中率；max_entry_size 在完整收集后判断 |
| P-06 | 访问日志 | body_length 取 Content-Length；chunked 请求 0、响应 NULL；WS/CONNECT 0；headers 重复键被覆盖；已修复 writer 退出 flush |
| P-07 | 统计 | total_ips 跨秒重复；sizes 依赖日志 Content-Length；保留期（7 天）仍硬编码；整周过期分区已 DROP，边界分区仍逐行 DELETE |
| P-08 | 上游 TLS | 只信 webpki + ca_file，不读系统 native roots；无 mTLS/SNI 覆盖 |
| P-09 | 面板 | 站点/证书/用户已可写；网关只读列表+API 删除；JWT 已改为 HttpOnly Cookie；无 refresh token/每用户 API key；无服务端分页；无静态托管；无前端测试 |
| P-13 | 证书 | 仅腾讯云 DNS-01；一张证书只能一个 DNS zone；ZeroSSL 必须配置 EAB；通配符未实测；ACME account key 可加密存储（需配置 `SECRET_ENCRYPTION_KEY`，否则明文） |
| P-10 | 负载均衡 | priority 为静态备用组；站点级健康检查/失败摘除已实现（主动+被动）；无跨节点健康状态/熔断 |
| P-11 | 管理面 | 无 CRUD、ACME/DNS、审计；有登录但无用户管理/RBAC 细分 |
| P-12 | 可观测性 | dashboard/gateway 已导出 Prometheus 指标；无 tracing 导出/Alertmanager；面板仅有基础统计 |

---

## 3. 未实现 Features

### 3.1 管理面 / 安全

- U-02 JWT refresh token / 短期令牌、每用户 API Key 管理（用户管理、RBAC、撤销已完成）
- U-05 审计日志、操作记录、配置回滚
- U-06 IP / User-Agent 拦截（按用户要求暂缓）
- U-07 限流（每 IP 令牌桶已实现）；WAF/CC/UA 规则与全局配额未实现
- U-08 请求体/头大小限制、连接数限制
- U-09 上游 mTLS、客户端证书
- U-10 面板安全头（API/health 已实现；面板静态 HTML 仍由托管层配置）

### 3.2 协议

- U-11 HTTP/3 流式 body、Alt-Svc、0-RTT、连接迁移
- U-12 gRPC / WebSocket over HTTP/3
- U-13 HTTP/2 extended CONNECT / WebSocket over h2
- U-15 gRPC-Web、CONNECT-UDP 等扩展
- U-16 HTTP/3 单连接并发请求限制/流控

### 3.3 可靠性 / 可观测性

- U-17 健康检查/失败自动切换（站点级已实现）；熔断、跨节点健康状态未实现
- U-19 Prometheus /metrics（dashboard/gateway 已实现）；tracing 导出与告警后端未实现
- U-20 配置热加载（证书/CA 目前需重启或 NOTIFY）
- U-21 统计保留期配置化（分区自动创建/删除已实现）
- U-22 访问日志落盘/导出指标（已实现重试 3 次 + 丢弃计数，未落盘/未暴露 /metrics）
- U-23 缓存持久化、多级缓存、single-flight、命中率
- U-24 OpenAPI / API 文档

### 3.4 前端

- U-26 服务端分页、排序、高级筛选
- U-28 图表时间范围选择、导出
- U-29 前端单测 / 组件测试 / 浏览器 E2E
- U-30 面板深链部署（history fallback）文档/配置

### 3.5 工程

- U-31 CI 中跑真实 E2E（DB、HTTP/2、HTTP/3、WS、gRPC）
- U-32 cargo audit / 依赖安全扫描
- U-33 分支合并策略：当前 13 个 feature 分支逐层堆叠，main 未集成
- U-34 PostgreSQL 18 beta + uint128 扩展的生产兼容性方案

---

## 4. Bugs / 风险

### P0 — 线上故障级

#### B-01 周分区只在迁移时创建（已修复）
- 症状：部署运行超过约 2 周后，新一周 access_* 分区不存在，日志 INSERT 失败。
- 原因：迁移 0001 仅在应用时执行一次 wg_ensure_upcoming_weeks(2)；运行期无定时调用。
- 修复：迁移 0003 新增 `wg_drop_expired_weekly_partitions()`；`shared::stats` 在启动后的第一个 tick
  以及随后每小时执行 `wg_ensure_upcoming_weeks(3)` + `wg_drop_expired_weekly_partitions(7)`。
- 验证：beta 库重启主控后 8 周前分区被整周 DROP，当前周 + 未来 3 周分区存在；
  集成测试 `crates/shared/tests/stats_rollup.rs` 覆盖。
- 位置：crates/shared/migrations/0003_stats_maintenance.sql、crates/shared/src/stats.rs

#### B-02 统计停机超过 90s 永久丢聚合（已修复）
- 症状：主控/统计任务停止超过约 90s，gap 内原始日志不再进入 access_stats_*；7 天后原始日志被删，统计永久缺失。
- 原因：秒级 rollup 只扫 now()-90s 到 now()-2s，无水位/补算。
- 修复：迁移 0003 新增 `stats_state`；每轮从 `LEAST(水位, now-90s)` 开始，缺口超过 1 小时的部分
  直接按分钟从原始日志回填，最近 1 小时写入秒表，然后正常归并；水位在同一事务内推进。
- 验证：beta 库把水位回拨 3 小时后，2 小时前的日志回填为分钟级 1 请求/1 响应/11、22 字节；
  集成测试覆盖 3 天缺口与重复执行不重复计数。上限为原始日志保留期 7 天。
- 位置：crates/shared/migrations/0003_stats_maintenance.sql、crates/shared/src/stats.rs

### P1 — 功能错误 / 数据 / 内存风险

#### B-03 WebSocket 到 https 后端不可用（已修复）
- 症状：backend URL 为 https 时 WS 握手失败；同时忽略 backend base path。
- 原因：upgrade.rs 使用裸 TcpStream::connect，不走 TLS；路径只用客户端 path。
- 修复：`AppState` 新增 `websocket_tls`（复用上游 CA 配置，ALPN 固定 http/1.1）；
  upgrade.rs 在 https 后端上做 TLS 握手后再执行 HTTP/1.1 upgrade；上游 target 由
  backend base path + 客户端 path/query 拼接（`backend_request_target`）。
- 验证（MCSManager daemon 4.19.3）：明文 ws、wss（测试 CA）、wss + 后端 `/mc` base path
  三种场景均完成 Socket.IO 握手/鉴权/`info/overview`/ping-pong；网关 34 个单测通过。
- 位置：gateway/src/upgrade.rs、gateway/src/state.rs

#### B-04 HTTP/3 绕过缓存 + 无 body 上限（部分修复，2026-11-20）
- 已修复：请求体声明/流式累计超过 `max_request_body_bytes` 返回 413；
  `handle_buffered` 响应超过 `max_response_body_bytes` 返回 502；不再无界 collect。
- 仍未做：h3 不查/写缓存；无流式请求/响应；h3 在途请求不参与优雅停机 drain。
- 实测：HTTP/1.1 2 KB body（限制 1 KB）返回 413；1 KB 以内正常转发。
- 位置：gateway/src/http3.rs、gateway/src/proxy.rs（handle_buffered）

#### B-05 停机丢日志 / 丢在途请求（已修复，2026-11-19）
- 修复：SIGTERM 后停止 accept → 用 Inflight 计数等待在途 HTTP/TCP 请求（上限 10s）→
  access writer 显式 shutdown 并 flush → 标记 offline → 关闭连接池。
- 实测：慢请求（3s）在内置 SIGTERM 后仍返回 200，访问日志入库，gateway 正常 exit 0 并 offline。
- 限制：HTTP/3 accept loop 仍直接 abort，QUIC 在途请求未做 drain。
- 位置：gateway/src/listeners.rs、gateway/src/access.rs、gateway/src/main.rs

#### B-06 日志通道满/批量失败丢数据（部分修复，2026-11-19）
- 已修复：写库失败重试 3 次（200/400ms 退避）；仍失败则记录 dropped/failed 计数并 WARN；
  channel full 每 100 条 WARN；优雅停机时 drain + flush。
- 仍未做：落盘 spill、Prometheus /metrics、告警；重启前最后一批仍可能丢失。
- 位置：gateway/src/proxy.rs（log_event）、gateway/src/access.rs

#### B-07 CONNECT 400/403 不记日志 + 端口开放（已修复，2026-11-20）
- 修复：handle_connect 在解析 authority 前创建 RequestLog；400/403/502/504/200 全部写访问日志；
  新增 `[server].connect_ports`（默认 `[443, 80]`，`GATEWAY_CONNECT_PORTS` 覆盖），
  端口不在白名单直接 403；空列表拒绝全部 CONNECT。
- 实测：`127.0.0.1:18443`（允许+站点命中）→ 200；`:18082`（端口不允许）→ 403；
  `unknown.test:18443`（站点不存在）→ 403；三条都有 CONNECT 访问日志（200/403/403）。
- 仍未做：CONNECT 鉴权 / mTLS；HTTP/2 extended CONNECT。
- 位置：gateway/src/upgrade.rs、gateway/src/config.rs、gateway/src/state.rs

### P2 — 行为 / 安全 / 统计偏差

#### B-08 统计 total_ips / sizes / 保留期偏差
- total_ips 为每秒 distinct 后求和，跨秒重复计数。
- 请求/响应 sizes 依赖日志 Content-Length，chunked 不准确。
- 保留期硬编码：原始 7d、秒 1h、分 1d、日长期。
- 原始日志只 DELETE 行，不 DROP 分区，磁盘膨胀。
- 位置：dashboard/backend/src/stats.rs、database.md 5.4

#### B-09 缓存行为限制（部分修复，2026-11-20）
- 已修复：缓存只在响应带 `Content-Length` 且不超过
  `min(cache.max_entry_size, max_response_body_bytes)` 时收集；chunked 响应流式转发、不缓存，
  不再“先完整收集再判断”。
- 仍存在：任意配置/证书 NOTIFY 清空全部缓存；无 single-flight；
  不支持 HEAD/If-Modified-Since/If-None-Match: */Vary/stale-while-revalidate。
- 位置：gateway/src/cache.rs、gateway/src/proxy.rs

#### B-10 上游 TLS 信任范围
- 只信 webpki + ca_file；不读系统 native roots；企业 CA 必须显式配置。
- 无 mTLS、无 SNI 覆盖。
- 位置：gateway/src/state.rs（upstream_connector）

#### B-11 无请求体/头/连接数限制（已修复全局层，2026-11-20）
- 修复：`max_connections`（TCP 全局信号量，超限丢连接）、`max_header_bytes`
  （HTTP/1 `max_buf_size` / HTTP/2 `max_header_list_size`）、`max_request_body_bytes`
  （Content-Length 超限 413 + `Limited` 流式包装）、`max_response_body_bytes`（缓存/h3 缓冲上限）。
- 实测：2 KB body → 413；20 KB header → 431；max_connections=1 时第二个连接被丢弃，第一个正常 200。
- 仍未做：站点级覆盖、连接/请求超时、HTTP/3 连接计数。
- 位置：gateway/src/config.rs、gateway/src/listeners.rs、gateway/src/proxy.rs、gateway/src/http3.rs

#### B-12 HTTP/3 无 Alt-Svc（已修复，2026-11-20）
- 修复：配置 `http3_ports` 时，TLS 的 HTTP/1.1 与 HTTP/2 响应自动带
  `Alt-Svc: h3="<udp_port>"; ma=86400`；明文 HTTP 与 h3 响应不加。
- 单测覆盖值格式（`h3=":8443"; ma=86400`）；TLS 场景未单独 E2E。
- 位置：gateway/src/state.rs、gateway/src/listeners.rs

#### B-13 gRPC 真实 E2E（已解决，2026-11-19）
- 已用 Nezha Dashboard v2.6.2 + agent v2.4.0 验证：h2c 与 TLS/ALPN h2 入口均注册成功，
  `ReportSystemInfo2` / `ReportSystemState` / `ReportGeoIP` 返回 200，`RequestTask` 双向流保持打开，
  Dashboard 会话 `ongoing = 1` 直到 Agent 停止；Go gRPC 客户端未报 trailer 错误。
- 仍无 CI 级 gRPC E2E（依赖外部软件，见 U-31）；访问日志只有 HTTP 200，不记录 grpc-status。

#### B-14 面板“今日”按 UTC 日界
- summary 的 date_trunc('day', now()) 是数据库 UTC 日；+08 用户看到的“今日”从本地 08:00 开始。
- 建议：按用户时区或站点时区聚合；至少文档说明。

#### B-15 面板认证安全（大部分修复，2026-11-20）
- 已修复：JWT 改为 `wg_session` HttpOnly + SameSite=Strict Cookie，不再进入 localStorage，
  登录 JSON 不再返回 token；面板启动用 `GET /api/auth/me` 恢复会话。
- 已修复：服务令牌使用恒定时间比较，支持 `[server].api_tokens` / `DASHBOARD_API_TOKENS`
  多令牌轮换（与旧 `api_token` 合并）。
- 已修复：/api 与 /health 响应带 CSP、nosniff、X-Frame-Options、Referrer-Policy、no-store；
  `X-Forwarded-Proto: https` 时追加 HSTS 与 Cookie `Secure`。
- 仍未做：服务令牌无过期/自动轮换；refresh token / 每用户 API Key；CSRF 仅依赖
  SameSite=Strict（未做 Origin 校验）；面板静态 HTML 的安全头由托管层配置。
- 位置：dashboard/backend/src/router.rs、config.rs；dashboard/frontend/src/session.ts、App.vue

#### B-29 TOTP 无重放保护/速率限制（已修复，2026-11-19）
- 修复：users.last_totp_step（迁移 0006）；登录用条件 UPDATE 保证同一 step 只接受一次，重放返回 401。
- 限速：进程内按 user+IP 计数，5 次失败/5 分钟锁 5 分钟，登录返回 429 + retry_after_secs。
- 实测：同 code 第二次 401；连续失败后即使 code 正确也 429。
- 限制：限速计数在进程内，多主控实例不共享；TOTP skew 内仍按 step 单调。

#### B-30 JWT 无撤销、RBAC 不细化（已修复，2026-11-19）
- 修复：users.token_version（迁移 0005）+ JWT claims 版本校验；logout/管理员 revoke/角色变更/重置 TOTP 都会 bump 版本，旧 JWT 立即 401。
- RBAC：admin（全部）、user（证书写）、view（只读）；路由分组 + 中间件强制。
- 仍需：服务令牌过期/自动轮换（多令牌手工轮换已支持）；refresh token / 每用户 API Key。

#### B-31 认证密钥暴露面（部分修复，2026-11-20）
- 已修复：users.totp_secret / jwt_secret、users_client_secrets.secret、证书私钥、ACME account key、
  DNS provider 凭据支持 AES-256-GCM 加密存储（`enc:v1:`，密钥来自 `SECRET_ENCRYPTION_KEY`）；
  dashboard 启动幂等加密历史明文，`SECRET_ENCRYPTION_KEY_PREVIOUS` 支持轮换期间解密。
- 仍未做：未设置密钥时保持明文；轮换后不自动重加密旧密文；默认管理员 TOTP secret 仍打印到
  WARN 日志一次；JWT secret 无过期/轮换策略。
- 位置：crates/shared/src/crypto.rs、dashboard/backend/src/secrets.rs、auth.rs、certificate.rs、
  gateway/src/sync.rs

#### B-16 面板 API 错误泄露（已修复，2026-11-20）
- 修复：internal() 记录完整错误 + 随机 error_id，客户端只收到
  `{"error":"internal server error","error_id":"..."}`；/health 数据库故障不再返回驱动错误。
- sqlx 错误链（anyhow chain 中的 sqlx::Error）一律 500 脱敏；用户/证书的校验错误保留原文。
- 实测：无效角色、重复用户名均为 400 原文；单测覆盖 internal()/auth_error() 脱敏。
- 位置：dashboard/backend/src/router.rs

#### B-17 面板无服务端分页/排序
- websites/certificates/gateways 全量返回；logs 只支持 limit<=500。
- 数据量大时性能和体验下降。

#### B-18 面板硬编码版本信息
- schema v5 / panel 0.1.0 / API 基址写死。
- 建议：从 /health 或构建注入。

#### B-19 升级连接无字节统计
- WebSocket/CONNECT body_length=0；无法做流量分析。

#### B-20 ETag/缓存校验限制
- 弱 ETag 使用 DefaultHasher（非加密哈希）。
- 不处理 If-Modified-Since、If-None-Match: *。
- 无 Date/Age 上游校正。

### P3 — 工程 / 运维

- B-21 分支堆叠未合并：ci/test-workflow → ... → feat/dashboard-auth，main 未集成；PR 未合并。
- B-22 CI 不跑 E2E：shared 集成测试在无 DATABASE_URL 时跳过；HTTP/3/WS/gRPC/CONNECT 仅本地验证。
- B-23 无 cargo audit / 前端依赖审计。
- B-24 PostgreSQL 18 beta + uint128 扩展的生产兼容/升级风险。
- B-25 无 metrics/告警（部分修复，2026-11-20）：dashboard/gateway 已导出 Prometheus 指标
  （缓存命中、日志丢弃、配置同步、连接、统计水位、登录失败等），并给出抓取与告警规则示例；
  仍未内置 Alertmanager/tracing 导出，上游错误率尚无独立指标。位置：gateway/src/metrics.rs、
  dashboard/backend/src/metrics.rs、deployment.md 第 11 节。
- B-26 面板静态托管/SPA fallback（已修复，2026-11-20：`DASHBOARD_STATIC_DIR` 直接托管面板；nginx 仍可作为替代）。
- B-27 无 CSP/HSTS/X-Frame-Options 等安全响应头（已修复，2026-11-20：API/health 与 dashboard 托管的面板均带安全头；外置 nginx 托管需自行配置）。
- B-28 无 OpenAPI/接口契约测试，前端类型手工维护。

---

## 5. 当前状态

- 最新分支：feat/health-checks（叠加在 feat/rate-limit 之后）
- 测试：网关 51、dashboard 25、shared 18 + 3 集成（含 crypto、stats 回填/分区回收）；前端 format/lint/type-check/build 通过
- 数据库：schema v6；2026-11-20 联调/统计/证书/CRUD/可靠性/资源上限/CONNECT/会话安全验证的临时数据已清理（websites/gateways/users/logs/stats = 0；测试证书已删除）
- 管理面：站点/网关/用户 CRUD + admin/user/view RBAC + token_version 撤销；面板站点/用户编辑页完成；
  会话改为 HttpOnly Cookie + 安全响应头，服务令牌可多值轮换，内部错误统一脱敏；
  dashboard 可直接托管面板（SIGTERM 30s drain），控制面 secret 支持 AES-256-GCM 静态加密；
  dashboard/gateway 提供 Prometheus `/metrics`（默认回环）；gateway 支持每 IP 令牌桶限流与站点级上游健康检查
- 证书：手动上传 + ACME（Let's Encrypt 实测签发；ZeroSSL 需 EAB）→ 网关 SNI 加载验证通过，见 `certificates.md`
- 实时协议：Nezha gRPC（h2c/TLS/双向流）与 MCSManager WS（101/鉴权/长连接）已用真实软件验证，见 `e2e-realtime.md`
- 面板：TOTP + JWT 登录；默认缓存 16 MiB（可配）；Vite 代理 /api → 127.0.0.1:3000

## 6. 建议优先级

1. ~~B-01 / B-02：分区续建 + 统计补算（P0）~~ ✅
2. ~~面板 CRUD + 用户管理/RBAC/JWT 撤销（U-01/U-02）~~ ✅；~~B-15/B-16/B-27（Cookie 会话、安全头、错误脱敏）~~ ✅；
   ~~B-31/P-13（密钥静态加密）~~ ✅（剩 refresh token / 每用户 API Key / 服务令牌过期 / 自动重加密）
3. ~~B-03~~ ✅ / ~~B-05~~ ✅ / ~~B-06~~ ✅ / ~~B-11~~ ✅ / ~~B-07~~ ✅ / ~~B-12~~ ✅ / B-04：h3 限制（body 上限已完成，仍缺缓存/流式）
4. U-11/U-13：h3 流式/Alt-Svc、h2 extended CONNECT
5. ~~U-19 metrics~~ ✅；~~U-07 每 IP 限流~~ ✅（WAF/CC 未做）；~~U-17 站点级健康检查~~ ✅（熔断未做）；
   U-23：缓存持久化/single-flight
6. B-21：合并分支、清理 PR 栈
