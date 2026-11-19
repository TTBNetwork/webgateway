# WebGateway v2 — Features & Issue Tracker

> 生成时间：2026-11-19  
> 截至提交：78f8222（feat/dashboard-panel；issue.md 本身未计入）  
> 验证环境：beta PostgreSQL（schema v2）、本地 E2E、GitHub Actions  
> 图例：✅ 已完成并验证；⚠️ 可用但有明确限制；❌ 未实现

---

## 1. 已实现 Features

### 1.1 平台 / 数据库 / 主控

| ID | Feature | 状态 | 说明 |
|---|---|---|---|
| F-01 | Rust workspace | ✅ | crates/shared、dashboard/backend、gateway；edition 2024 |
| F-02 | CI 流水线 | ✅ | fmt、clippy -D warnings、workspace 单测、前端 lint/build；CI run 44545263511 全绿 |
| F-03 | PostgreSQL schema v2 | ✅ | 迁移 0001/0002，schema_state 版本标记，_sqlx_migrations |
| F-04 | 数据库表结构 | ✅ | users/configs/certificates/DNS/websites/gateways、4 张访问日志表、秒/分/日统计表 |
| F-05 | 周分区 | ✅ | access_* 四张表按周分区；迁移时预建未来两周 |
| F-06 | NOTIFY 触发器 | ✅ | websites/certificates/configurations/dns_providers 的 INSERT/UPDATE/DELETE |
| F-07 | 主控独占迁移 | ✅ | advisory lock；网关只等待 schema_state，不执行 DDL |
| F-08 | 主控 /health | ✅ | 返回数据库状态与 schema 版本 |
| F-09 | 统计 rollup | ✅ | 主控 10s 任务；90s 窗口、2s lag；秒→分→日；事务级 advisory lock |
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
| F-19 | gRPC 上游 | ⚠️ | h2c/TLS、application/grpc 检测、TE: trailers、无 60s 超时；无真实 gRPC E2E |
| F-20 | WebSocket 升级 | ⚠️ | h1 Upgrade、101、双向桥接；仅 http 上游 |
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
| F-32 | 缓存 | ⚠️ | 64MiB LRU、TTL/类型白名单、ETag/304、HIT/MISS/Age；h3 不缓存 |

### 1.4 管理面板

| ID | Feature | 状态 | 说明 |
|---|---|---|---|
| F-33 | 只读 API | ✅ | /api/summary、websites、certificates、gateways、logs |
| F-34 | API 鉴权 | ⚠️ | DASHBOARD_API_TOKEN + Bearer；未设置则开发模式开放 |
| F-35 | Vue 3 面板 | ✅ | 登录、概览、站点、证书、网关、日志、设置 |
| F-36 | 视觉 / 交互 | ✅ | 深/浅色、中/英、响应式侧栏、统计卡片、SVG 流量图 |
| F-37 | 前端工程 | ✅ | 自写 history router + 原生 fetch；无新增依赖；package/lock 未改 |
| F-38 | 面板验证 | ✅ | Vite 代理 401/200 + 真实数据；format/lint/build 通过 |

### 1.5 测试

| ID | Feature | 状态 | 说明 |
|---|---|---|---|
| F-39 | 网关单测 | ✅ | 33 个；fmt/clippy 全通过 |
| F-40 | shared 测试 | ✅ | 10 单测 + 2 集成（临时库） |
| F-41 | 本地 E2E | ✅ | NOTIFY、headers、PROXY、路由、日志、统计、缓存、TLS、WS、CONNECT、h3、权重 |

---

## 2. 部分完成 / 有明确限制

| ID | Feature | 限制 |
|---|---|---|
| P-01 | HTTP/3 | 请求/响应全缓冲，无流式；无 Alt-Svc；无 0-RTT/连接迁移；h3 不走缓存；WS/gRPC over h3 未实现；无 body 上限 |
| P-02 | gRPC | 无真实 gRPC server E2E，trailers 为设计假设 |
| P-03 | WebSocket | https 后端不可用（裸 TcpStream，不走 TLS connector）；忽略 backend base path；字节数记 0 |
| P-04 | CONNECT | 仅 h1；400/403 不写日志；已配置 host 的任意端口可隧道；无鉴权 |
| P-05 | 缓存 | 仅 h1/h2；配置同步清空全部；无持久化/Vary/HEAD/stale-while-revalidate/single-flight/命中率；max_entry_size 在完整收集后判断 |
| P-06 | 访问日志 | body_length 取 Content-Length；chunked 请求 0、响应 NULL；WS/CONNECT 0；headers 重复键被覆盖；writer 退出丢最后一批 |
| P-07 | 统计 | total_ips 跨秒重复；sizes 依赖日志 Content-Length；保留期硬编码；原始日志只删行不 drop 分区 |
| P-08 | 上游 TLS | 只信 webpki + ca_file，不读系统 native roots；无 mTLS/SNI 覆盖 |
| P-09 | 面板 | 只读；共享静态令牌，无用户/RBAC；令牌存 localStorage；无服务端分页；无静态托管；无前端测试 |
| P-10 | 负载均衡 | priority 为静态备用组；无健康检查/失败自动切换 |
| P-11 | 管理面 | 无 CRUD、ACME/DNS、审计、用户体系 |
| P-12 | 可观测性 | 无 /metrics、无 tracing 导出；面板仅有基础统计 |

---

## 3. 未实现 Features

### 3.1 管理面 / 安全

- U-01 站点/证书/网关/用户 CRUD API（当前只读）
- U-02 用户登录、会话、RBAC、API Key 管理
- U-03 ACME 自动签发/续期、DNS provider 集成
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
- U-14 上游 WebSocket TLS（wss）+ base path
- U-15 gRPC-Web、CONNECT-UDP 等扩展
- U-16 HTTP/3 单连接并发请求限制/流控

### 3.3 可靠性 / 可观测性

- U-17 健康检查、失败自动切换、熔断
- U-18 优雅停机 / 连接 drain（当前直接 abort）
- U-19 Prometheus /metrics、tracing 导出、告警
- U-20 配置热加载（证书/CA 目前需重启或 NOTIFY）
- U-21 统计保留期配置化、分区自动创建/删除
- U-22 访问日志重试/落盘/丢弃指标
- U-23 缓存持久化、多级缓存、single-flight、命中率
- U-24 OpenAPI / API 文档

### 3.4 前端

- U-25 站点/证书 CRUD 表单与校验
- U-26 服务端分页、排序、高级筛选
- U-27 用户/角色管理页面
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

#### B-01 周分区只在迁移时创建
- 症状：部署运行超过约 2 周后，新一周 access_* 分区不存在，日志 INSERT 失败。
- 原因：迁移 0001 仅在应用时执行一次 wg_ensure_upcoming_weeks(2)；运行期无定时调用。
- 影响：访问日志丢失；缓存/统计不影响。
- 临时规避：定期手工执行 SELECT wg_ensure_upcoming_weeks(2);
- 建议：主控 stats 任务周期性 ensure + drop 过期分区。
- 位置：crates/shared/migrations/0001_initial_schema.sql、dashboard/backend/src/stats.rs

#### B-02 统计停机超过 90s 永久丢聚合
- 症状：主控/统计任务停止超过约 90s，gap 内原始日志不再进入 access_stats_*；7 天后原始日志被删，统计永久缺失。
- 原因：秒级 rollup 只扫 now()-90s 到 now()-2s，无水位/补算。
- 临时规避：不要长时间停主控；或手工补算。
- 建议：记录 last_aggregated_at，按分钟/小时补算；或直接以分钟为粒度补。
- 位置：dashboard/backend/src/stats.rs

### P1 — 功能错误 / 数据 / 内存风险

#### B-03 WebSocket 到 https 后端不可用
- 症状：backend URL 为 https 时 WS 握手失败；同时忽略 backend base path。
- 原因：upgrade.rs 使用裸 TcpStream::connect，不走 HttpsConnector；路径只用客户端 path。
- 影响：wss 上游、带路径前缀的 WS 服务不可用。
- 建议：复用 HttpsConnector；拼接 backend base path；补 E2E。
- 位置：gateway/src/upgrade.rs

#### B-04 HTTP/3 绕过缓存 + 无 body 上限
- 症状：h3 请求不查/写缓存；请求体 BytesMut 与响应 collect 无上限。
- 影响：缓存命中率下降；大 body 可导致内存压力/DoS。
- 建议：h3 接缓存或明确文档；加 max_request_body/max_response_body；流式化。
- 位置：gateway/src/http3.rs、gateway/src/proxy.rs（handle_buffered）

#### B-05 停机丢日志 / 丢在途请求
- 症状：SIGTERM 时 listeners.abort_all、log_writer.abort、h3 task abort，无 drain。
- 影响：最后一批访问日志、在途代理请求丢失。
- 建议：停止接受新连接 → 等待 in-flight → flush 日志 → 退出。
- 位置：gateway/src/main.rs

#### B-06 日志通道满/批量失败丢数据
- 症状：try_send 满时 debug 丢弃；写库失败整批丢。
- 影响：高峰/DB 抖动时日志缺失，且无指标。
- 建议：重试/落盘/丢弃计数；至少暴露指标与告警。
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

#### B-13 gRPC 无真实 E2E
- content-type 检测、h2 客户端、trailers 透传均未在真实 grpc server 上验证。
- 建议：CI 起 grpc-go/tonic echo server 做 E2E。

#### B-14 面板“今日”按 UTC 日界
- summary 的 date_trunc('day', now()) 是数据库 UTC 日；+08 用户看到的“今日”从本地 08:00 开始。
- 建议：按用户时区或站点时区聚合；至少文档说明。

#### B-15 面板令牌安全
- 静态共享 token；localStorage 存储；未设置 DASHBOARD_API_TOKEN 时 /api 完全开放（开发模式）。
- 无用户/RBAC、无速率限制/锁定；token 比较非恒定时间。
- 建议：用户登录 + HttpOnly Cookie/短时 token；生产强制设置 token；加 CSP。

#### B-16 面板 API 错误泄露
- internal() 把数据库 error.to_string() 返回给客户端。
- 建议：对外返回通用错误，详细错误只记日志。
- 位置：dashboard/backend/src/router.rs

#### B-17 面板无服务端分页/排序
- websites/certificates/gateways 全量返回；logs 只支持 limit<=500。
- 数据量大时性能和体验下降。

#### B-18 面板硬编码版本信息
- schema v2 / panel 0.1.0 / API 基址写死。
- 建议：从 /health 或构建注入。

#### B-19 升级连接无字节统计
- WebSocket/CONNECT body_length=0；无法做流量分析。

#### B-20 ETag/缓存校验限制
- 弱 ETag 使用 DefaultHasher（非加密哈希）。
- 不处理 If-Modified-Since、If-None-Match: *。
- 无 Date/Age 上游校正。

### P3 — 工程 / 运维

- B-21 分支堆叠未合并：ci/test-workflow → ... → feat/dashboard-panel，main 未集成；PR 未合并。
- B-22 CI 不跑 E2E：shared 集成测试在无 DATABASE_URL 时跳过；HTTP/3/WS/gRPC/CONNECT 仅本地验证。
- B-23 无 cargo audit / 前端依赖审计。
- B-24 PostgreSQL 18 beta + uint128 扩展的生产兼容/升级风险。
- B-25 无 metrics/告警：日志丢弃、缓存命中、统计延迟、上游错误率均不可观测。
- B-26 面板无静态托管/SPA fallback 配置；生产需 nginx 配置。
- B-27 无 CSP/HSTS/X-Frame-Options 等安全响应头。
- B-28 无 OpenAPI/接口契约测试，前端类型手工维护。

---

## 5. 当前状态

- 最新分支：feat/dashboard-panel
- 最新提交：dc5b666（API + Vue 面板）、78f8222（docs）；issue.md 为后续文档提交
- CI：44545263511 全绿（针对 78f8222）
- 测试：网关 33、shared 10 + 2 集成；前端 format/lint/build 通过
- 数据库：schema v2；测试数据已清理（websites/certs/gateways/logs = 0）
- 面板：只读；Vite 代理 /api → 127.0.0.1:3000；DASHBOARD_API_TOKEN 控制鉴权

## 6. 建议优先级

1. B-01 / B-02：分区续建 + 统计补算（P0）
2. 面板 CRUD + 用户登录/RBAC（U-01/U-02）
3. B-03 / B-04 / B-05 / B-06：WS TLS、h3 限制、优雅停机、日志可靠性
4. U-11/U-13：h3 流式/Alt-Svc、h2 extended CONNECT
5. U-17/U-19/U-23：健康检查、metrics、缓存持久化
6. B-21：合并分支、清理 PR 栈
