# Changelog

## 2026-11-18

### CI 测试流程（已提交并推送，分支 `ci/test-workflow`）
- 新增 `.github/workflows/ci.yml`：detect、rust-fmt、rust-check、frontend-lint、frontend-build、ci-success。
- 新增可复用复合 action：`.github/actions/setup-rust`、`.github/actions/setup-frontend`。
- 前端提交 `dashboard/frontend/pnpm-lock.yaml`，新增 `lint:check` / `format:check` 脚本。
- 新增 `rust-toolchain.toml`（stable + rustfmt/clippy），`.gitignore` 忽略 `target/`。
- 验证：GitHub Actions run 44347070816 通过；Rust 任务因当时没有 `Cargo.toml` 按设计跳过。

### 数据库设计（`database.md`，待确认）
- 新增数据库设计文档：表结构、主控独占初始化、网关等待就绪、迁移与测试方案。
- 按内部 PostgreSQL 镜像（`postgresql-dockerfile`：postgres:18 + Pigsty `pg-uint128`）调整：
  使用 `uint128` 扩展的 `uint2[]` / `uint8` / `uint2` 类型与 `btree_gin`；扩展由主控在迁移开头创建。
- 实测 beta 库（全程 `BEGIN ... ROLLBACK`，未留数据）：PostgreSQL 18.2 空库；`CREATE EXTENSION` 可在事务内执行；
  `uint2[]`、`uint8` 默认值、`uint2 CHECK`、`GIN (TEXT)`（btree_gin）均验证通过。

### 数据库主控（已实施，分支 `feat/database`）
- 新增 Rust workspace：`crates/shared` + `dashboard/backend`，提交 `Cargo.lock`。
- `crates/shared`：`DatabaseConfig`（TOML + `DATABASE_*` 环境变量）、连接池、编译期内嵌 sqlx 迁移、
  主控迁移（advisory lock + `lock_timeout`）、`verify_schema`、`wait_for_master_schema`；日志与错误信息全部英文。
- `crates/shared/migrations/0001_initial_schema.sql`：`uint128` / `btree_gin` 扩展、`schema_state` 就绪标记、
  配置/用户/证书/DNS/站点、`gateways` 上下线注册表、四张周分区访问日志、秒/分/日三级统计表、
  配置表 NOTIFY 通道与 `updated_at` 触发器。不迁移 v1 数据。
- `dashboard` 主控：启动时连接 -> 迁移 -> 校验 -> axum `GET /health`；`config.example.toml`；`config.toml` 已 gitignore。
- `access_response_logs.status` 按用户要求不加 RFC 范围 CHECK。
- 测试：9 个单元测试 + 临时库集成测试（主控初始化、幂等、网关等待超时/就绪）。
- 验证：在 `webgateway_beta_v2` 实际初始化成功（schema version 1、16 张表、4 周分区、两个扩展已装），
  `GET /health` 返回 200 `{"status":"ok","schema_version":1}`。
### 时区与技术栈规则（2026-11-18）
- TimeZone 统一 `UTC+0`：beta 库执行 `ALTER DATABASE webgateway_beta_v2 SET timezone='UTC'`；
  `postgresql-dockerfile` 的 CMD 增加 `-c timezone=UTC`；集成测试断言主控/网关会话均为 UTC。
- 面板后续按用户时区展示（前端换算，服务端不做本地时区换算）。
- 记录网关缓存（主控下发策略：大小上限、文件类型、ETag、TTL 等，细节后续）与 IP/UA 拦截（优先级低）；
  缓存/拦截配置后续放 `websites.config` JSONB，本步不改 schema。
- AGENT.md 增加上述规则；database.md 更新时区约定与 `websites.config` 说明。
### 网关（分支 `feat/gateway`）
- 新增 `gateway` crate：等待主控 schema；`gateways` 注册 online、10s 心跳、SIGTERM/Ctrl-C 优雅 offline。
- 每 10s 全量同步 websites/certificates；监听端口取配置与 `websites.ports` 的并集，随同步增删。
- HTTP/HTTPS 反向代理：Host 精确/通配符路由、`match_path` + 轮询、`X-Forwarded-*`、60s 超时、404/502/504。
- TLS：数据库 PEM 证书 + SNI 解析 + 默认自签证书 + ALPN h2/http1.1（E2E 实测 HTTP/2）。
- 响应增加 `Server: WebGateway` 与 `X-WebGateway-Node: <country-city>`。
- 单元测试 9 个；beta 库 E2E 验证 HTTP/HTTPS/404/在线/心跳/离线（测试数据已清理）。
- 文档 `gateway.md`，`database.md` 更新实施状态。
- 未实现（下一步）：访问日志与统计、缓存、IP/UA 拦截、WebSocket、上游 TLS。
### 网关增强（分支 `feat/gateway-notify`）
- 配置同步改为 LISTEN/NOTIFY：订阅 `websites_updater` / `certificates_updater`；
  迁移 0002 增加配置表 DELETE 通知；监听连接（重）连时用 `(COUNT(*), MAX(updated_at))` 水位对比，
  只有变化才全量同步；**移除 10s 全量轮询**。
- 代理请求头下沉到 `websites.config.headers`（set/remove，占位符 `$remote_addr` / `$scheme` / `$host`）；
  未配置时使用默认四个头，逐跳头始终剔除。
- `X-WebGateway-Node` 改为 `<country-city>-<fnv1a64(location+machine_id) 16hex>`。
- schema version 2（beta 库已由主控应用）；shared 迁移测试与网关单测更新。
- E2E：NOTIFY 增/删 1s 内生效、站点 headers 回显验证、节点哈希；测试数据已清理。
- WebSocket / gRPC / HTTP3 / 特殊 RFC 列为下一阶段（gateway.md 有拆分说明）。
### 网关客户端 IP / PROXY protocol（分支 `feat/gateway-proxy`）
- 新增 PROXY protocol v1/v2 解析（在 TLS/HTTP 之前消费）；站点级 `get_request_ip`：
  `raw` / `proxyProtocol` / `xForwardedFor` / `xRealIP`。
- 解析出的客户端 IP 用于 `$remote_addr` 与上游 X-Real-IP / X-Forwarded-For；CDN 场景可用 XFF/X-Real-IP 模式。
- 实测：无头 / v1 / v2 连接分别得到 127.0.0.1 / 198.51.100.7 / 203.0.113.9。
- 网关单测 19 个；文档更新。gRPC（h2/h2c + trailers）上游传输、path/UA/params 路由列为下一阶段。
### 网关路由与 gRPC 传输（分支 `feat/gateway-routing`）
- 后端匹配规则：`match.path` glob、`match.user_agent` glob、`match.query` map、`match.method`；
  全部 AND，命中轮询，无命中回退全部。
- gRPC：`content-type: application/grpc*` 使用 HTTP/2-only 客户端（h2c），保留 `TE: trailers` 并透传 trailers，
  不套 60s 超时。
- E2E：POST `/api/x?version=v2` + curl UA → upstream-one；version/method/UA 不匹配或 `/web` → upstream-two。
- 单测 23 个；文档更新。WebSocket/HTTP3/特殊 RFC、访问日志/缓存/IP-UA 拦截仍待做。
### 网关访问日志（分支 `feat/gateway-access-logs`）
- 新增 `gateway/src/access.rs`：请求/响应/秒级 size 四张表写入；500ms 或 256 条批量事务；时间取 DB `now()`。
- proxy 覆盖 200/404/502/504 路径，记录 headers JSON、客户端 IP、website_id、status、Content-Length。
- E2E：GET/POST/404 产生 request/response 行；POST 5 字节、响应 12/9 字节，size 表按秒写入。
- 限制：chunked 长度为 0/NULL；writer 退出时最后一批可能丢；`access_stats_*` 聚合与保留期未做。
- 网关单测 26 个；文档更新。
### 网关 WebSocket / CONNECT（分支 `feat/gateway-upgrades`）
- WebSocket：`Upgrade: websocket` 走升级通路；服务端 `serve_connection_with_upgrades`，
  上游 HTTP/1.1 握手 + `.with_upgrades()`，101 后双向桥接；非 101 正常回传。
- CONNECT：authority host 必须命中已配置站点，否则 403；200 后原始 TCP 隧道（非 TLS MITM）。
- 访问日志记录 101/200；E2E WS echo（hello-websocket）与 CONNECT（localhost:9500）通过。
- 网关单测 28 个；未做 HTTP/3、HTTP/2 extended CONNECT、上游 TLS。
### 网关 HTTP/3（分支 `feat/gateway-http3`）
- 引入 quinn/h3/h3-quinn；`[server].http3_ports` / `GATEWAY_HTTP3_PORTS` 开启 UDP 监听；
  TLS 1.3 + ALPN `h3`，复用 SNI 证书存储（含默认自签证书）。
- 请求/响应完整缓冲后经 buffered HTTP/1.1 client 转发上游；访问日志记录 `HTTP/3.0`。
- E2E：aioquic 客户端 GET `/h3` → 200 `http3-upstream-ok`。
- 限制：无流式、单连接串行、WebSocket/gRPC over h3 未做、未调 0-RTT/连接迁移。
- 网关单测 28 个。
### 统计聚合与保留期（分支 `feat/stats-rollup`）
- 主控新增 stats 任务：每 10s、事务级 advisory lock；重算最近 90s（2s lag）秒级统计，UPSERT 幂等。
- 秒 → 分（超过 1 小时）、分 → 日（跨天）用 `DELETE ... RETURNING` 原子归并并删除旧粒度。
- 原始访问日志保留 7 天，主控每小时清理；聚合秒表保留 1 小时、分钟表保留 1 天、日表长期。
- E2E：最近日志秒级 30/150 字节；旧秒归并分钟 70/700；旧分归并日 90/900。
- 说明：`total_ips` 为每秒去重后求和；`backend_error_requests` 以 502/504 计。
### 网关缓存（分支 `feat/gateway-cache`）
- 站点级 `websites.config.cache`：`enabled` / `ttl` / `content_types` / `max_entry_size`。
- 64 MiB 内存 LRU；仅缓存 GET + 200 + 无 Set-Cookie/Vary/no-store|private|no-cache 的响应；
  请求带 Authorization/Cookie 时跳过。
- 命中 `X-WebGateway-Cache: HIT` + `Age`；未命中 `MISS`；ETag/If-None-Match → 304。
- 配置同步（NOTIFY）时清空缓存；E2E：MISS count=1 → HIT count=1 → 304 → 配置更新后 MISS count=2。
- 网关单测 32 个；限制：无持久化、HEAD/POST 不缓存、不支持 stale-while-revalidate/Vary。
### 上游 TLS（分支 `feat/upstream-tls`）
- 用 hyper-rustls `HttpsConnector` 替换 `HttpConnector`；支持 `http://` 与 `https://` 后端，
  ALPN 由 connector 自动配置（h2/http/1.1）。
- `[upstream_tls].ca_file` / `GATEWAY_UPSTREAM_CA_FILE` 追加 PEM CA；webpki 根始终受信。
- gRPC 与 buffered 客户端复用同一 connector，支持 gRPC over TLS / HTTPS 缓冲请求。
- E2E：本地 CA + `https://localhost:9443` 后端 → 200 `upstream-https-ok`。
- 网关单测 32 个；修复 hyper-rustls 预置 ALPN 导致的 panic。
### 后端权重/优先级 + HTTP/3 并发（分支 `feat/backend-weights`）
- 后端新增 `weight`（默认 1）与 `priority`（默认 0，越小越优先）；低优先级组内按权重轮询，
  更高 priority 作为备用组（无自动故障切换）。
- HTTP/3 每个请求独立 spawn，同一 QUIC 连接可并发处理多个流（body 仍缓冲）。
- E2E：40 次请求按 3:1 分到 30/10，备用组 0；HTTP/3 同连接 5 个请求全部 200、按权重 4/1。
- 网关单测 33 个；文档更新。
### 控制面板 v1（分支 `feat/dashboard-panel`）
- 主控新增只读 API：`/api/summary`、`/api/websites`、`/api/certificates`、`/api/gateways`、`/api/logs`；
  设置 `DASHBOARD_API_TOKEN` 后要求 Bearer 令牌。
- Vue 3 + TypeScript 面板：登录、概览（统计卡片 + 24h 流量图）、站点、证书、网关、访问日志、设置。
- 深/浅色、中/英、响应式侧边栏；登录令牌存 localStorage。
- 无新增前端运行时依赖：自写 history router + 原生 fetch，`package.json` / `pnpm-lock.yaml` 未改。
- 验证：Vite 代理 401/200、真实数据、`pnpm format:check` / `lint:check` / `build` 通过；后端 fmt/clippy/test 通过。
- 当前面板只读；CRUD、用户体系、ACME、静态托管后续。
### 面板认证 + 缓存默认值（分支 `feat/dashboard-auth`）
- 主控 TOTP/JWT 登录：空 users 表自动创建默认 admin（TOTP secret 仅首次 WARN 日志打印）；
  `POST /api/auth/login` 校验用户名 + 6 位码，签发每用户 HS256 JWT（默认 7 天）。
- `DASHBOARD_API_TOKEN` 保留为服务令牌；所有 /api 默认需要 JWT 或服务令牌。
- 面板登录改为用户名 + TOTP，JWT/用户信息存 localStorage；设置页显示账号/角色。
- 缓存默认 64 MiB → **16 MiB**，新增 `[server].cache_max_size_bytes` / `GATEWAY_CACHE_MAX_SIZE`。
- 认证限制（TOTP 重放/限速、JWT 撤销/RBAC、secret 明文）已记录到 issue.md。
- 验证：admin + TOTP → JWT → /api/summary；dashboard 2 个单测；前端 build/lint 通过。

## 2026-11-19（实时协议联调）

### Nezha gRPC 与 MCSManager WebSocket 联调（分支 `test/realtime-protocols`）
- 新增 `scripts/e2e/mcsmanager_ws_probe.mjs`：零依赖（Node 全局 WebSocket）Socket.IO v4 探针，
  经网关完成 engine.io 握手、命名空间连接、daemon key 鉴权、`info/overview` 调用，
  并保持连接跨过 engine.io ping 周期（默认 20s），验证长连接与 ping/pong。
- 新增 `scripts/e2e/nezha_grpc_probe.sh`：用真实 nezha-agent 经网关连接 Nezha Dashboard，
  轮询 Dashboard SQLite 的 `servers` / `online_sessions`，确认 gRPC 会话保持 `ongoing = 1` 且无 RPC 错误。
- 新增 `e2e-realtime.md`：完整拓扑、站点配置、复现步骤、实测结果与限制。
- 实测环境与结果：
  - Nezha Dashboard v2.6.2 + agent v2.4.0：gRPC h2c（18080）与 TLS/ALPN h2（18443）均注册成功；
    `ReportSystemInfo2` / `ReportSystemState` / `ReportGeoIP` 返回 200，`RequestTask` 双向流保持打开，
    Dashboard 会话 `ongoing = 1` 约 37s，Agent 停止后正常结束；Go gRPC 客户端未报 trailer 错误。
  - MCSManager v10.22.1（daemon 4.19.3）：升级 101、Socket.IO 握手、daemon key 鉴权、
    `info/overview`（daemon 4.19.3）、45s 内两次 engine.io ping/pong 全部通过。
  - 同一 MCS daemon 开启 SSL 后：直连 wss 成功，经网关失败，复现 B-03（upgrade.rs 走裸 TcpStream，不支持 https 上游）。
- `gateway.md` 更新 WebSocket 限制与验证清单；`issue.md` 将 F-19/F-20 标为已用真实软件验证，
  P-02/P-03/B-03/B-13 补充实测结论。
- 本分支仅新增测试脚本与文档，未改生产代码。
