# WebGateway v2 — Features & Issue Tracker

> 生成时间：2026-11-19  
> 截至提交：78f8222（feat/dashboard-panel；issue.md 本身未计入）  
> 验证环境：beta PostgreSQL（schema v3）、本地 E2E、GitHub Actions  
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
| F-34 | API 鉴权 | ✅ | TOTP + 每用户 JWT；admin/user/view RBAC；token_version 撤销（logout/revoke/角色变更/重置 TOTP）；服务令牌视为 admin；无 refresh token / 每用户 API key |
| F-35 | Vue 3 面板 | ✅ | 登录、概览、站点 CRUD、证书 CRUD、网关、日志、用户管理、设置 |
| F-36 | 视觉 / 交互 | ✅ | 深/浅色、中/英、响应式侧栏、统计卡片、SVG 流量图 |
| F-37 | 前端工程 | ✅ | 自写 history router + 原生 fetch；无新增依赖；package/lock 未改 |
| F-38 | 面板验证 | ✅ | Vite 代理 401/200 + 真实数据；format/lint/build 通过 |

### 1.5 测试

| ID | Feature | 状态 | 说明 |
|---|---|---|---|
| F-39 | 网关单测 | ✅ | 36 个；fmt/clippy 全通过 |
| F-40 | shared 测试 | ✅ | 14 单测 + 3 集成（临时库，含 stats 回填/分区回收、schema v6） |
| F-41 | 本地 E2E | ✅ | NOTIFY、headers、PROXY、路由、日志、统计、缓存、TLS、WS、CONNECT、h3、权重、TOTP/JWT；Nezha gRPC 与 MCSManager WS 联调；证书上传与 Let's Encrypt 真实签发；优雅停机 drain + TOTP 重放/限速 |

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

---

## 2. 部分完成 / 有明确限制

| ID | Feature | 限制 |
|---|---|---|
| P-01 | HTTP/3 | 请求/响应全缓冲，无流式；无 Alt-Svc；无 0-RTT/连接迁移；h3 不走缓存；WS/gRPC over h3 未实现；无 body 上限 |
| P-02 | gRPC | 已在 Nezha 上验证；访问日志只记 HTTP 200，无法区分 grpc-status；无 gRPC-Web |
| P-03 | WebSocket | 升级连接字节数记 0；无 mTLS / SNI 覆盖；wss 与后端 base path 已支持（2026-11-19） |
| P-04 | CONNECT | 仅 h1；400/403 不写日志；已配置 host 的任意端口可隧道；无鉴权 |
| P-05 | 缓存 | 仅 h1/h2；配置同步清空全部；无持久化/Vary/HEAD/stale-while-revalidate/single-flight/命中率；max_entry_size 在完整收集后判断 |
| P-06 | 访问日志 | body_length 取 Content-Length；chunked 请求 0、响应 NULL；WS/CONNECT 0；headers 重复键被覆盖；已修复 writer 退出 flush |
| P-07 | 统计 | total_ips 跨秒重复；sizes 依赖日志 Content-Length；保留期（7 天）仍硬编码；整周过期分区已 DROP，边界分区仍逐行 DELETE |
| P-08 | 上游 TLS | 只信 webpki + ca_file，不读系统 native roots；无 mTLS/SNI 覆盖 |
| P-09 | 面板 | 站点/证书/用户已可写；网关只读列表+API 删除；JWT 存 localStorage；无 refresh token/每用户 API key；无服务端分页；无静态托管；无前端测试 |
| P-13 | 证书 | 仅腾讯云 DNS-01；一张证书只能一个 DNS zone；ZeroSSL 必须配置 EAB；通配符未实测；ACME account key 明文入库 |
| P-10 | 负载均衡 | priority 为静态备用组；无健康检查/失败自动切换 |
| P-11 | 管理面 | 无 CRUD、ACME/DNS、审计；有登录但无用户管理/RBAC 细分 |
| P-12 | 可观测性 | 无 /metrics、无 tracing 导出；面板仅有基础统计 |

---

## 3. 未实现 Features

### 3.1 管理面 / 安全

- U-02 JWT refresh token / 短期令牌、每用户 API Key 管理（用户管理、RBAC、撤销已完成）
- U-04 面板静态托管 / SPA fallback / nginx 配置
- U-05 审计日志、操作记录、配置回滚
- U-06 IP / User-Agent 拦截（按用户要求暂缓）
- U-07 限流、WAF、CC 防护
- U-08 请求体/头大小限制、连接数限制
- U-09 上游 mTLS、客户端证书
- U-10 面板安全头（CSP/HSTS/X-Frame-Options）

### 3.2 协议

- U-11 HTTP/3 流式 body、Alt-Svc、0-RTT、连接迁移
- U-12 gRPC / WebSocket over HTTP/3
- U-13 HTTP/2 extended CONNECT / WebSocket over h2
- U-15 gRPC-Web、CONNECT-UDP 等扩展
- U-16 HTTP/3 单连接并发请求限制/流控

### 3.3 可靠性 / 可观测性

- U-17 健康检查、失败自动切换、熔断
- U-19 Prometheus /metrics、tracing 导出、告警
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

#### B-04 HTTP/3 绕过缓存 + 无 body 上限
- 症状：h3 请求不查/写缓存；请求体 BytesMut 与响应 collect 无上限。
- 影响：缓存命中率下降；大 body 可导致内存压力/DoS。
- 建议：h3 接缓存或明确文档；加 max_request_body/max_response_body；流式化。
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

#### B-07 CONNECT 400/403 不记日志 + 端口开放
- 症状：authority 缺失/非法返回 400、站点不匹配返回 403，均无 access log；已配置 host 任意端口可隧道。
- 影响：审计缺失；SSRF/滥用风险。
- 建议：所有返回路径统一 log_failure；加端口白名单/鉴权。
- 位置：gateway/src/upgrade.rs（handle_connect）

### P2 — 行为 / 安全 / 统计偏差

#### B-08 统计 total_ips / sizes / 保留期偏差
- total_ips 为每秒 distinct 后求和，跨秒重复计数。
- 请求/响应 sizes 依赖日志 Content-Length，chunked 不准确。
- 保留期硬编码：原始 7d、秒 1h、分 1d、日长期。
- 原始日志只 DELETE 行，不 DROP 分区，磁盘膨胀。
- 位置：dashboard/backend/src/stats.rs、database.md 5.4

#### B-09 缓存行为限制
- 任意配置/证书 NOTIFY 都会清空全部站点缓存（非按站点）。
- 无 single-flight，热点过期时并发回源。
- max_entry_size 在完整收集 body 后才判断。
- 不支持 HEAD/If-Modified-Since/If-None-Match: */Vary/stale-while-revalidate。
- 位置：gateway/src/cache.rs、gateway/src/proxy.rs

#### B-10 上游 TLS 信任范围
- 只信 webpki + ca_file；不读系统 native roots；企业 CA 必须显式配置。
- 无 mTLS、无 SNI 覆盖。
- 位置：gateway/src/state.rs（upstream_connector）

#### B-11 无请求体/头/连接数限制
- 大 body、慢连接、大量并发可消耗资源；h3 更明显。
- 建议：全局/站点级限制与超时。

#### B-12 HTTP/3 无 Alt-Svc
- h1/h2 响应不广播 h3，客户端不会自动升级，必须显式连接 UDP 端口。
- 建议：在 TLS 响应加 Alt-Svc: h3=":443"; ma=86400。

#### B-13 gRPC 真实 E2E（已解决，2026-11-19）
- 已用 Nezha Dashboard v2.6.2 + agent v2.4.0 验证：h2c 与 TLS/ALPN h2 入口均注册成功，
  `ReportSystemInfo2` / `ReportSystemState` / `ReportGeoIP` 返回 200，`RequestTask` 双向流保持打开，
  Dashboard 会话 `ongoing = 1` 直到 Agent 停止；Go gRPC 客户端未报 trailer 错误。
- 仍无 CI 级 gRPC E2E（依赖外部软件，见 U-31）；访问日志只有 HTTP 200，不记录 grpc-status。

#### B-14 面板“今日”按 UTC 日界
- summary 的 date_trunc('day', now()) 是数据库 UTC 日；+08 用户看到的“今日”从本地 08:00 开始。
- 建议：按用户时区或站点时区聚合；至少文档说明。

#### B-15 面板认证安全
- JWT 存 localStorage，存在 XSS 读取风险；无 CSP。
- 静态服务令牌仍可用于 API；服务令牌比较为非恒定时间。
- 建议：HttpOnly Cookie 或短时 token、加 CSP、恒定时间比较。

#### B-29 TOTP 无重放保护/速率限制（已修复，2026-11-19）
- 修复：users.last_totp_step（迁移 0006）；登录用条件 UPDATE 保证同一 step 只接受一次，重放返回 401。
- 限速：进程内按 user+IP 计数，5 次失败/5 分钟锁 5 分钟，登录返回 429 + retry_after_secs。
- 实测：同 code 第二次 401；连续失败后即使 code 正确也 429。
- 限制：限速计数在进程内，多主控实例不共享；TOTP skew 内仍按 step 单调。

#### B-30 JWT 无撤销、RBAC 不细化（已修复，2026-11-19）
- 修复：users.token_version（迁移 0005）+ JWT claims 版本校验；logout/管理员 revoke/角色变更/重置 TOTP 都会 bump 版本，旧 JWT 立即 401。
- RBAC：admin（全部）、user（证书写）、view（只读）；路由分组 + 中间件强制。
- 仍需：服务令牌（DASHBOARD_API_TOKEN）过期/轮换；refresh token / 每用户 API Key。

#### B-31 认证密钥暴露面
- 默认管理员 TOTP secret 只在首次创建时打印 WARN 日志；日志集中后可能泄露。
- users.totp_secret / jwt_secret 明文存库（沿用 v1）；JWT secret 未轮换。
- 建议：一次性初始化流程/加密存储/密钥轮换。

#### B-16 面板 API 错误泄露
- internal() 把数据库 error.to_string() 返回给客户端。
- 建议：对外返回通用错误，详细错误只记日志。
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
- B-25 无 metrics/告警：日志丢弃、缓存命中、统计延迟、上游错误率均不可观测。
- B-26 面板无静态托管/SPA fallback 配置；生产需 nginx 配置。
- B-27 无 CSP/HSTS/X-Frame-Options 等安全响应头。
- B-28 无 OpenAPI/接口契约测试，前端类型手工维护。

---

## 5. 当前状态

- 最新分支：feat/reliability-hardening（叠加在 feat/dashboard-rbac 之后）
- 测试：网关 36、dashboard 10、shared 14 + 3 集成（含 stats 回填/分区回收）；前端 format/lint/type-check/build 通过
- 数据库：schema v6；2026-11-19 联调/统计/证书/CRUD/可靠性验证的临时数据已清理（websites/gateways/users/logs/stats = 0；测试证书已删除）
- 管理面：站点/网关/用户 CRUD + admin/user/view RBAC + token_version 撤销；面板站点/用户编辑页完成
- 证书：手动上传 + ACME（Let's Encrypt 实测签发；ZeroSSL 需 EAB）→ 网关 SNI 加载验证通过，见 `certificates.md`
- 实时协议：Nezha gRPC（h2c/TLS/双向流）与 MCSManager WS（101/鉴权/长连接）已用真实软件验证，见 `e2e-realtime.md`
- 面板：TOTP + JWT 登录；默认缓存 16 MiB（可配）；Vite 代理 /api → 127.0.0.1:3000

## 6. 建议优先级

1. ~~B-01 / B-02：分区续建 + 统计补算（P0）~~ ✅
2. ~~面板 CRUD + 用户管理/RBAC/JWT 撤销（U-01/U-02）~~ ✅（剩 refresh token/API key/服务令牌轮换）
3. ~~B-03~~ ✅ / ~~B-05~~ ✅ / ~~B-06~~ ✅ / B-04：h3 限制（仍缺 body 上限/缓存）
4. U-11/U-13：h3 流式/Alt-Svc、h2 extended CONNECT
5. U-17/U-19/U-23：健康检查、metrics、缓存持久化
6. B-21：合并分支、清理 PR 栈
