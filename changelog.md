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

### 统计维护：分区续建 + 水位补算（分支 `feat/stats-maintenance`）
- 迁移 0003 `crates/shared/migrations/0003_stats_maintenance.sql`：`stats_state` 单行水位表
  （created_at/updated_at + 触发器）、`wg_drop_expired_weekly_partitions(days)`（只 DROP 整周都过期的分区，
  < 7 天拒绝）、`wg_ensure_upcoming_weeks(3)`，schema version → **3**。
- 统计任务从 `dashboard/backend/src/stats.rs` 移到 `crates/shared/src/stats.rs`（主控启动，便于集成测试）：
  - 每轮按 `stats_state.last_aggregated_at` 计算窗口，且总是至少重算最近 90s（保留旧行为，延迟批次仍可重放）；
  - 水位缺口超过 1 小时的部分直接按分钟从原始日志回填，最近 1 小时写秒表，随后秒→分→日；
  - 水位推进与所有聚合在同一个事务内提交；首个 tick 立即执行分区续建/回收，之后每小时一次；
  - 原始日志保留 7 天不变：整周过期分区整表 DROP，边界分区仍逐行 DELETE。
- `verify_schema` 增加 `stats_state` 表与 `last_aggregated_at` 列检查；单元测试覆盖迁移 0003；
  新增集成测试 `crates/shared/tests/stats_rollup.rs`（3 天缺口回填、最近 10s 进秒表、重复执行不重复计数、
  8 周前分区 DROP、当前周 + 未来 3 周存在）。
- 实测 beta 库：水位回拨 3 小时后，2 小时前的原始日志回填为分钟级 1 请求/1 响应、11/22 字节；
  重启主控后 8 周前分区被 DROP；schema version 3；测试数据已清理。
- 文档：`database.md` 5.2 / 5.4 / 6 / 9 更新；`issue.md` 将 B-01/B-02 标为已修复，
  更新 F-03 / F-05 / F-09 / P-07 / U-21。

### WebSocket 上游 TLS 与 base path（分支 `fix/gateway-websocket-tls`）
- `gateway/src/upgrade.rs` 不再对 https 后端发明文 HTTP：
  - `state.rs` 抽出 `upstream_tls_config()`，HTTP 客户端与 WebSocket 升级共用同一 CA 配置；
  - `AppState` 新增 `websocket_tls`（ALPN http/1.1），wss 后端先 TLS 握手再执行 HTTP/1.1 upgrade；
  - 上游 target 改为 backend base path + 客户端 path/query（`backend_request_target`），
    修复忽略后端路径前缀的问题。
- 新增单测：base path 拼接（带/不带尾斜杠、无 base path）；网关单测 33 → 34。
- E2E（MCSManager daemon 4.19.3）：
  - 明文 ws（`/socket.io`）：PASS；
  - wss（测试 CA 签发的 CA:FALSE leaf，`GATEWAY_UPSTREAM_CA_FILE`）：PASS；
  - wss + 后端 `/mc` base path：客户端 `/socket.io/...` 映射到上游 `/mc/socket.io/...`，PASS。
- 注意：rustls 拒绝 `CA:TRUE` 自签证书作为服务端 leaf（`CaUsedAsEndEntity`），测试证书需由 CA 签发。
- 文档：`gateway.md` 3/5.2/7/8、`e2e-realtime.md`、`issue.md`（B-03 已修复，P-03/U-14 更新）。

### 主控证书管理（分支 `feat/certificates`）
- 迁移 0004 `certificate_issuance.sql`：`certificates.auto_renew` / `dns_zone` / `last_error`，
  续期 partial index；schema version → **4**。
- 新增 `dashboard/backend/src/certificate.rs`：
  - 手动上传：解析 PEM 证书（SAN、notAfter）与私钥，校验 hostname/SAN 覆盖后入库；
  - ACME：每 60s 调度，`signing_started_at` 10 分钟租约 + `FOR UPDATE SKIP LOCKED`；
    到期前 30 天自动续期；ZeroSSL（配置 EAB 时优先）→ Let's Encrypt；DNS-01 腾讯云 DNSPod；
    ACME account key 按 CA 持久化到 `configurations`；失败写 `last_error`，约 10 分钟后重试；
  - API：`POST /api/certificates`（上传）、`POST /api/certificates/issue`、
    `POST /api/certificates/{id}/issue`、`DELETE /api/certificates/{id}`、
    `GET/POST/DELETE /api/dns-providers`（不回显密钥）。
- 面板：证书页新增自动签发 / 手动上传表单、重新签发、删除、pending/error 展示；i18n 中英。
- 凭据：`scripts/source-dnsprovider.sh` 解析 `.secrets/dnsprovider` 的 `NAME = value`
  （空格/引号/export 都支持），`scripts/run-dashboard.sh` 直接启动主控；`.secrets/` 已加入
  .gitignore，值从不打印。支持 `TENCENT_KEY/TENCENT_SECRET` 等别名；ZeroSSL EAB、ACME_EMAIL、
  HTTPS_PROXY、ACME_DNS_PROPAGATION_SECS 均可通过环境变量配置。
- 新增 `vendor/acmex`（0.8.0，MIT OR Apache-2.0），修复上游在真实 CA 上不可用的问题：
  去掉 FIPS feature、JWS JSON 序列化、ES256/EC P-256 账户 key、ECDSA DER→raw、
  newAccount 响应解析、POST-as-GET 空 payload、CSR 空 subject、EAB、DNS-01 按 identifier
  生成 TXT、传播等待与失败清理。
- 实测（beta 库，测试域名 `cert-test2.atxa.top`）：
  - 手动上传证书 → DB → 网关 SNI 返回上传的 CN/SAN；
  - Let's Encrypt DNS-01：账户复用、TXT 创建/删除、订单 ready→valid、签发 90 天证书
    （issuer `C=US, O=Let's Encrypt, CN=YE1`）；网关通过 NOTIFY 加载并由
    `https://cert-test2.atxa.top:18443` 返回 200；
  - ZeroSSL 未配置 EAB，按设计跳过并回退 LE；
  - 面板 format/lint/type-check/build 通过；dashboard 5 单测、shared 12 + 3 集成。
- 文档：新增 `certificates.md`（API、凭据、流程、vendor 补丁、限制）；`database.md` 更新 schema v4；
  `issue.md` 新增 F-42/F-43/F-44、P-13，更新 U-01/U-03/U-25/P-09。

### 管理面：站点/用户 CRUD、RBAC 与 JWT 撤销（分支 `feat/dashboard-rbac`）
- 迁移 0005 `user_token_version.sql`：`users.token_version INTEGER NOT NULL DEFAULT 0`；schema version → **5**。
- `auth.rs`：JWT claims 增加 `token_version`；认证时与数据库比对，logout/管理员撤销/角色变更/TOTP 重置
  都会 bump 版本，旧 JWT 立即 401；新增用户管理（列表/创建/改角色/删除/重置 TOTP/撤销）、
  `is_admin` / `can_write_certificates` 与角色校验；last-admin 保护。
- `router.rs`：路由拆成 read / certificate-write / admin 三组；
  - `view` 只读；`user` 可管理证书；`admin` 全量；服务令牌视为 admin；
  - 新增 `POST /api/auth/logout`、`/api/users`（GET/POST/PATCH/DELETE）、
    `/api/users/{id}/totp`、`/api/users/{id}/revoke`、站点 CRUD、网关更新/删除。
- 新增 `websites.rs`：hosts/ports/backends/config 校验（URL scheme、权重/优先级、JSON 对象）、
  `int[]::uint2[]` 写入、创建/更新/删除。
- 面板：新增用户管理页（admin 导航）、站点新增/编辑/删除表单（仅 admin 显示）、
  退出时先调用服务端 logout 撤销 JWT；i18n 中英。
- 验证（beta 库）：服务令牌创建站点/用户；view JWT 读 200、写 403；revoke/logout 后旧 JWT 401；
  角色更新 bump token_version；禁止删除最后一个 admin；站点创建/更新/删除成功（ports 正确写入）。
- 文档：`database.md` schema v5、`issue.md` F-33/F-34/F-45/F-46、B-30 已修复、P-09/U-01/U-02 更新。

### 认证与可靠性加固（分支 `feat/reliability-hardening`）
- 迁移 0006 `totp_replay.sql`：`users.last_totp_step BIGINT`；schema version → **6**。
- TOTP 防重放：登录时取匹配 step，条件 UPDATE 保证同一 step 只接受一次；重放返回 401。
- 登录限速：进程内按 user+IP 计数，5 次失败/5 分钟锁 5 分钟；登录返回 429 + `retry_after_secs`。
- 访问日志可靠性：
  - 写库失败重试 3 次（200/400ms 退避）；仍失败记录 dropped/failed 计数并 WARN；
  - channel full 每 100 条 WARN；
  - `AccessWriter` 新增显式 shutdown：drain 队列 + flush 后再退出。
- 优雅停机：
  - `Listeners` 增加 watch shutdown + in-flight 计数；SIGTERM 后停止 accept，
    最多等待 10s 让 HTTP/TCP 在途请求完成；
  - 顺序：停止 accept → drain → abort h3 accept → flush access log → offline → 关闭连接池。
- 实测：
  - 同一个 TOTP code 第二次登录 401；连续失败后正确 code 也返回 429；
  - 慢请求（3s）在内置 SIGTERM 后仍返回 200，访问日志写入数据库，gateway exit 0 且 gateways 标记 offline；
  - 日志显示 `access log writer stopped count=1`，无丢失。
- 限制：HTTP/3 在途请求未做 drain（accept loop abort）；日志无落盘 spill/metrics 端点；
  登录限速为单实例内存计数。
- 测试：gateway 36、dashboard 10、shared 14 + 3 集成；clippy/fmt 通过。
- 文档：`database.md` schema v6、`issue.md` B-05/B-06/B-29 更新、F-47/F-48 新增。

### 资源上限：请求体 / header / 连接数（分支 `feat/limits-hardening`）
- 配置新增（`gateway/config.example.toml`）：
  - `max_connections`（默认 4096）：TCP 监听共享信号量，超限直接关闭新连接并 WARN；
  - `max_header_bytes`（默认 32 KiB，最小 8 KiB）：HTTP/1 `max_buf_size` +
    HTTP/2 `max_header_list_size`；
  - `max_request_body_bytes`（默认 64 MiB）：Content-Length 超限转发前 413，
    chunked/无长度用 `Limited` 包装；
  - `max_response_body_bytes`（默认 64 MiB）：缓存与 HTTP/3 缓冲上限。
  - 均支持环境变量 `GATEWAY_MAX_*`。
- 上游 client body 类型改为 boxed body，使请求体可被 `Limited` 包装（不再只能接受 `Incoming`）。
- 缓存修复：仅在响应带 `Content-Length` 且不超过
  `min(cache.max_entry_size, max_response_body_bytes)` 时收集；chunked 响应流式转发、不缓存
  （不再先完整 collect 再判断）。
- HTTP/3：请求体超过上限返回 413（声明/流式累计均检查）；`handle_buffered` 响应超过上限返回 502；
  h3 仍不接入缓存。
- 实测（max_connections=1，max_header_bytes=8192，max_request_body=1024）：
  - 2 KB body → **413 Payload Too Large**；512 B body 正常转发到后端（后端返回 501 属预期）；
  - 20 KB header → **431**；
  - 并发两个请求时第二个被丢弃（curl exit 56），第一个 3 s 慢请求正常 200；
  - 开启站点缓存后首次 MISS（3.0 s），第二次 HIT（0.001 s）→ 缓存未被破坏。
- 限制：站点级覆盖/超时/HTTP/3 连接计数未做；h3 仍无流式与缓存；h3 在途请求不参与停机 drain。
- 测试：gateway 36、dashboard 10、shared 14 + 3 集成；clippy/fmt 通过。
- 文档：`gateway.md` 2.1 资源限制、5.3 HTTP/3；`issue.md` B-04/B-09/B-11、F-49/F-50。

### CONNECT 审计/端口白名单 + HTTP/3 Alt-Svc（分支 `feat/connect-alt-svc`）
- CONNECT 审计：`handle_connect` 在解析 authority 前创建访问日志；400（authority 缺失/非法）、
  403（端口/站点不允许）、502/504（上游失败/超时）、200（隧道建立）全部落库；
  200 行记录 website_id，失败行 website_id 为 NULL。
- CONNECT 端口白名单：新增 `[server].connect_ports`（默认 `[443, 80]`），
  `GATEWAY_CONNECT_PORTS` 覆盖；空列表拒绝全部 CONNECT。端口不在列表直接 403，不再任意端口可隧道。
- HTTP/3 Alt-Svc：配置 `http3_ports` 时，TLS 的 h1/h2 响应自动插入
  `Alt-Svc: h3=":<udp_port>"; ma=86400`（state 构建、listener 注入）；明文 HTTP 与 h3 自身不加。
- `AppState::new` 参数收敛为 `StateLimits`，避免 clippy too_many_arguments。
- 实测：
  - CONNECT `127.0.0.1:18443`（白名单 + 站点）→ 200；
  - `127.0.0.1:18082`（端口不允许）→ 403；
  - `unknown.test:18443`（站点不存在）→ 403；
  - 三条均写入访问日志（200/403/403，body_length 9）。
  - Alt-Svc 单测覆盖值格式；TLS 场景未单独 E2E。
- 测试：gateway 37、dashboard 10、shared 14 + 3 集成；clippy/fmt 通过。
- 文档：`gateway.md` 2.1/5.2/5.3、`gateway/config.example.toml`、`issue.md` B-07/B-12、F-51/F-52。

### 面板会话安全 + 安全响应头 + API 错误脱敏（分支 `feat/dashboard-session-security`）
- 会话改为 HttpOnly Cookie：`POST /api/auth/login` 不再在 JSON 返回 JWT，而是设置
  `wg_session`（HttpOnly、SameSite=Strict、Path=/、Max-Age=会话 TTL）；JWT 不再进入
  localStorage，XSS 无法读取。面板启动时调用 `GET /api/auth/me` 恢复会话。
- `DASHBOARD_TOKEN_EXPIRES`（默认 7 天）改为配置项 `session_ttl_secs`，clamp 到
  300..2592000 秒；签发时同时写入 JWT `exp` 与 Cookie `Max-Age`。
- 服务令牌支持轮换：`[server].api_tokens` / `DASHBOARD_API_TOKENS`（逗号分隔）可与旧
  `api_token` / `DASHBOARD_API_TOKEN` 并存；逐个恒定时间比较（不提前返回），便于新旧令牌重叠过渡。
- 安全响应头（所有 /health 与 /api 响应）：CSP
  `default-src 'none'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'`、
  `X-Content-Type-Options: nosniff`、`X-Frame-Options: DENY`、`Referrer-Policy: no-referrer`、
  `Cache-Control: no-store`、`Permissions-Policy`；`X-Forwarded-Proto: https` 时追加 HSTS 与 Cookie `Secure`。
- 认证失败：带无效/过期/已撤销 Cookie 的 401 同时下发清除 Cookie（Max-Age=0），浏览器不再反复重放失效会话。
- API 错误脱敏：`internal()` 记录完整错误 + `error_id`，客户端只收到
  `{"error":"internal server error","error_id":...}`；`/health` 数据库故障不再返回驱动错误；
  用户/证书接口的校验错误保留原文，数据库错误按 500 脱敏（检测 anyhow 链中的 `sqlx::Error`）。
- 前端：移除 localStorage token 与 Authorization 头，改用同源 Cookie；新增启动引导
  （`session.ready`）避免登录页闪烁；设置页文案更新为 HttpOnly 会话。
- 实测（beta DB，端口 13000）：
  - 登录 200 返回 `user` + `expires_in`，`Set-Cookie` 含 HttpOnly/SameSite=Strict/Max-Age=600，HTTP 下无 Secure；
  - 带 Cookie `GET /api/auth/me` 200；无凭证 401；
  - `X-Forwarded-Proto: https` 时 Cookie 带 Secure 且响应带 HSTS；
  - 两个服务令牌均 200，错误令牌 401；重启后只保留新令牌 → 旧令牌 401、新令牌 200（轮换）；
  - 退出后旧 Cookie 401 且响应清除 Cookie（token_version 提升，该用户所有会话失效）；
  - 校验错误仍返回 400 原文（无效角色、重复用户名）。
- 测试：dashboard 20（新增 router 8 + config 2）、gateway 37、shared 14 + 3 集成；
  fmt/clippy 通过；前端 format/lint/type-check/build 通过。
- 限制：Cookie 认证要求面板与 API 同源（需反向代理）；未实现 refresh token / 每用户 API Key /
  服务令牌过期；面板静态 HTML 的安全头由托管层负责；CSRF 依赖 SameSite=Strict，未额外做 Origin 校验。
- 文档：`issue.md` B-15/B-16/B-27/B-30、F-53、P-09；`dashboard/frontend/README.md`；
  `dashboard/backend/config.example.toml`；`certificates.md` API 认证说明。

### 生产部署：面板静态托管 + SIGTERM 优雅停机（分支 `feat/production-deploy`）
- 新增 `DASHBOARD_STATIC_DIR`（`[server].static_dir`）：dashboard 可直接托管构建后的面板，
  `/api` 与 `/health` 优先，其余路径 SPA 回退到 `index.html`；未知 `/api/*` 仍返回 JSON 404。
- 静态资源安全：路径穿越校验（拒绝 `..`、反斜杠、NUL）；按扩展名设置 Content-Type；
  `/assets/*` 返回 `Cache-Control: public, max-age=31536000, immutable`，`index.html` 为 `no-store`。
- CSP 按响应类型区分：面板 HTML 使用 `default-src 'self'` 策略（同源脚本 + 内联样式），
  JSON API 保持 `default-src 'none'`；静态资源保留自身缓存策略，API 仍 no-store。
- 优雅停机：dashboard 同时处理 SIGTERM 与 Ctrl-C，停止 accept 并最多等待 30 秒；随后退出调度任务。
- 配置校验：`DASHBOARD_STATIC_DIR` 必须存在且包含 `index.html`，否则启动失败；`tokio` 增加 `fs` feature。
- 实测（端口 13000/13001）：
  - `/` 200 HTML + 面板 CSP + `no-store`；`/websites` SPA 回退 index；`/assets/*.js` immutable；
  - `/api/nope` 404 JSON；`/assets/../../Cargo.toml` 不泄露文件（返回 index）；
  - SIGTERM 后 exit 0、8ms 退出、端口释放，日志记录 received SIGTERM / dashboard stopped。
- 新增 `deployment.md`：拓扑、构建、数据库备份/迁移、环境变量表、nginx/systemd 示例、
  首次上线检查表与已知生产限制清单。
- 测试：dashboard 23（新增 3）；fmt/clippy 通过。
- 文档：`issue.md` B-26/B-27/U-04、F-54；`dashboard/backend/config.example.toml`；
  `dashboard/frontend/README.md`。

### 密钥静态加密 + 修复 dashboard 停机超时（分支 `feat/production-deploy`）
- 新增 `shared::crypto`：AES-256-GCM（ring），存储格式 `enc:v1:<base64url(nonce||ciphertext)>`；
  密钥来自 `SECRET_ENCRYPTION_KEY`（64 hex 或 base64 的 32 字节），`SECRET_ENCRYPTION_KEY_PREVIOUS`
  仅用于解密过渡；未配置密钥时保持明文并 WARN。
- 加密范围：`users.totp_secret` / `users.jwt_secret`、`users_client_secrets.secret`、
  `certificates.private_key`（`fullchain` 公开不加密）、`configurations` 的 `acme_account:*` key_pem、
  `dns_providers.provider` 的 `secret_id`/`secret_key`。
- dashboard 启动执行幂等迁移 `secrets::encrypt_legacy_secrets`，把历史明文一次性加密
  （日志 `encrypted plaintext secrets count=N`）；gateway 读取证书前解密，失败时跳过并告警。
- 修复本轮 E2E 发现的回归：dashboard 的 30s 停机超时误包住了整个 serve 生命周期，
  导致无信号时启动 30s 后必退；现在超时只作用于收到 SIGTERM 后的 drain。
- 实测（beta DB）：
  - 先以无密钥启动写入 2 用户 / 1 证书 / 1 DNS provider（全部明文），重启带密钥 → `count=4`，
    DB 全部 `enc:v1:`，DNS JSON 中明文 secret_id 消失；admin 登录（TOTP/JWT 解密）200；
  - gateway 带密钥启动 → `certificate_hosts=1`，`curl --resolve` 200，证书 `CN=secure-test.atxa.top`；
  - gateway 无密钥 → `cannot decrypt the certificate key`，证书数 0，回落到 rcgen 自签；
  - dashboard 无密钥访问已加密数据 → 登录 500 + error_id，日志 `failed to decrypt the TOTP secret`；
  - 修复后 dashboard 持续运行超过 30s（旧实现会自行退出）。
- 测试：shared 18（+4 crypto）、dashboard 23、gateway 37；fmt/clippy 通过。
- 文档：`deployment.md` 4.1 密钥加密与迁移顺序/轮换、gateway 环境变量、首次上线检查表；
  `issue.md` B-31/P-13、F-55；`certificates.md`、两个 `config.example.toml`。

### Prometheus 指标（分支 `feat/observability`）
- 新增 `gateway/src/metrics.rs`：独立 plain-HTTP `/metrics` 监听（`metrics_port` /
  `GATEWAY_METRICS_PORT`，`metrics_bind` 默认 `127.0.0.1`），不占用站点端口、无需认证。
  指标：请求/HTTP3/WS/CONNECT 计数、缓存 hit/miss/expired/store/eviction/rejected/clears 与
  entries/bytes/max、活跃连接与在途请求、访问日志 dropped/failed/queue、配置同步成功/失败、
  路由/证书 host 数、`webgateway_info`。
- 新增 `dashboard/backend/src/metrics.rs`：独立 `/metrics` 监听（`DASHBOARD_METRICS_PORT`，
  默认回环），输出 `dashboard_info`、database_up、uptime、站点/证书/网关/用户计数、
  证书 30 天内到期数、统计水位与 lag、登录失败/限流、API 内部错误计数。
- 计数器为进程内 AtomicU64；缓存计数在 `cache.rs` 内记录，连接数在 listener 生命周期记录，
  同步结果在 gateway 主循环记录，dashboard 登录/错误计数在 router 记录。
- 新增 `deployment.md` 第 11 节：抓取配置、指标清单、告警规则示例（DashboardDown/GatewayDown/
  AccessLogDropping/ConfigSyncFailing/CacheHitRatioLow/StatsLagging/CertificateExpiring 等）。
- 实测：
  - dashboard `/metrics` 绑定 127.0.0.1:19100，`dashboard_database_up 1`、`dashboard_info{schema_version="6"}`；
    错误 TOTP 后 `dashboard_auth_failures_total 1`；
  - gateway `/metrics` 绑定 127.0.0.1:19101；同一站点请求两次 → `cache_misses_total 1`、
    `cache_hits_total 1`、`cache_stores_total 1`、`requests_total 2`、`route_hosts 1`；
  - 监控端口只监听回环，未出现在公网 listener。
- 测试：gateway 39（+2）、dashboard 25（+2）；fmt/clippy 通过。
- 文档：`deployment.md` 环境变量/限制/监控章节、`gateway.md` 2.2、两个 `config.example.toml`、
  `issue.md` B-25/P-12/U-19、F-56。

### 每客户端 IP 限流（分支 `feat/rate-limit`）
- 新增 `gateway/src/ratelimit.rs`：令牌桶限流，配置 `[server.rate_limit]`
  （`enabled` / `requests_per_second` / `burst` / `max_tracked_ips`），
  环境变量 `GATEWAY_RATE_LIMIT_RPS`（>0 自动启用）、`_BURST`、`_MAX_TRACKED`。
- 客户端 IP 使用站点 `client_ip` 配置解析（raw / proxyProtocol / xForwardedFor / xRealIP），
  与访问日志 `remote_addr` 一致；在站点命中后、缓存/上游之前执行。
- HTTP/1.1、HTTP/2、HTTP/3、WebSocket、CONNECT 全部接入；超限返回 **429** +
  `Retry-After: <秒>`，并写访问日志（status=429）。
- 状态：16 个分片 `Mutex<HashMap<IpAddr, Bucket>>`，全局 tracked 计数；表满时先清理
  >60s 未访问条目，仍满则放行（fail-open），避免误伤全部流量。`burst=0` 默认 `ceil(rps)`，
  `max_tracked_ips` 下限 1024；非法配置启动失败。
- 指标：新增 `webgateway_rate_limited_total`、`webgateway_rate_limit_tracked_ips`。
- 实测（GATEWAY_RATE_LIMIT_RPS=1、BURST=2）：
  - 同一 IP 连续 4 次 → 200/200/429/429，429 带 `Retry-After: 1`；
  - 换源 IP（127.0.0.2）→ 200，说明桶按 IP 隔离；
  - 指标 `rate_limited_total 3`、`tracked_ips 2`，访问日志出现 status=429。
- 测试：gateway 39 → 45（限流 6 个单测：burst/refill、IP 隔离、全局跟踪、过期清理、配置校验）；
  fmt/clippy 通过。
- 限制：单节点计数，多节点需在边缘外做全局配额；限流配置不支持 NOTIFY 热更新。
- 文档：`gateway.md` 2.3 与配置/环境变量、`deployment.md`、`config.example.toml`、
  `issue.md` U-07/F-57。

### 上游健康检查与失败摘除（分支 `feat/health-checks`）
- 新增 `gateway/src/health.rs`：站点级 `websites.config.health_check`
  （enabled / path / interval_secs / timeout_secs / unhealthy_threshold / healthy_threshold，
  默认关闭；非法 path/interval/timeout 会 WARN 并禁用该站点检查）。
- 主动探测：调度器每秒扫描启用检查的站点，按 interval 对每个 backend 发 GET
  `<backend><path>`；状态码 <500 视为健康，连接失败/超时/5xx 计失败。
- 被动标记：请求路径上的连接失败/超时计入失败（仅健康检查开启的站点），恢复由主动探测完成。
- 后端选择：优先健康后端，保持原有 path/rules/weight/priority 语义；
  若候选全部不健康则回退完整集合，避免全站 502；priority 备用组在首选组不健康时自动生效。
- 状态：进程内 `BackendHealth`（site id + backend URL 为 key，原子计数），配置同步/删除站点后清理；
  探测任务随停机 abort；未探测过的后端默认 healthy。
- 指标：`webgateway_health_check_success_total`、`webgateway_health_check_failure_total`、
  `webgateway_backends_unhealthy`。
- 实测（gateway 端口 18085/18086/18087，死后端 18081、活后端 18080）：
  - 开启健康检查的站点 6 次请求 **全部 200**（死后端被摘除）；
  - 未开启健康检查的对照站点 6 次请求 **502/200 交替**（证明死后端平时会被选中）；
  - 日志出现 `upstream health check failed backend=http://127.0.0.1:18081/`；
    指标 `health_check_success_total 3`、`failure_total 3`、`backends_unhealthy 2`。
- 测试：gateway 45 → 51（+6 health 单测：阈值/恢复、失败重置、探测间隔、清理、配置校验）；
  fmt/clippy 通过。
- 限制：健康状态单节点、无跨节点共享/熔断/半开；探测为 GET，可能在上游产生访问记录；4xx 视为健康。
- 文档：`gateway.md` 2.2/5.5/8、`deployment.md` 限制与告警/上线检查表、`issue.md` P-10/U-17/F-58。

### HTTP/3 流式响应 + 停机 drain（分支 `feat/h3-streaming`）
- h3 响应改为从上游**流式转发**：`handle_buffered` 返回 `Response<GatewayBody>`，
  HTTP/3 端逐帧 `send_data`，不再整体 collect；大下载内存占用为单 chunk。
  `max_response_body_bytes` 不再限制 h3 响应（缓存路径仍使用该上限）。
- h3 请求路径改用 `state.client`（支持流式响应体），请求体仍完整缓冲并受
  `max_request_body_bytes` 限制（超限 413）。
- 停机 drain：新增 `http3::Http3` 句柄（endpoints + accept loop tasks）；SIGTERM 时先
  `stop_accepting`（停止新 QUIC 连接，不断开在途流），h3 请求与 TCP 连接共享
  `listeners::Inflight` 计数，`shutdown` 最多等待 10s，随后 `endpoint.close()` 并 abort accept loop。
- 实测：
  - h3 GET 64 KiB 响应（`GATEWAY_MAX_RESPONSE_BODY=1024`）→ **200 + 完整 65536 字节**
    （旧实现会 502）；
  - 3s 慢 h3 请求在 1s 后收到 SIGTERM：客户端 **200 + 7 字节**，gateway exit 0，
    日志 `shutdown signal received`、access log flush 后 offline，端口释放。
- 限制：h3 请求体仍整体缓冲；h3 仍不走缓存；WS/gRPC over h3、0-RTT/连接迁移未实现。
- 测试：gateway 51（现有 h3/代理/限流/健康检查单测全部通过）、dashboard 25、shared 18 + 3；
  fmt/clippy 通过。
- 文档：`gateway.md` 5.3、`deployment.md` 限制表、`issue.md` P-01/B-04/F-59。

### 缓存 Vary / HEAD / 304（分支 `feat/cache-vary`）
- `gateway/src/cache.rs` 重构为按 `Vary` 变体存储：
  - 条目携带响应 `Vary` 列出的请求头值（规范化为小写 HeaderName + 请求值），查找时逐项比较；
  - 同一 key 可保存多个变体（gzip/br/无编码等），LRU 顺序按 `(key, token)` 维护；
  - `Vary: *` 直接拒绝缓存（计入 `cache_rejected_total`）；替换/淘汰/清空/占用统计均按变体计数。
- `proxy.rs`：
  - GET 与 HEAD 都查缓存；仅 GET 响应写入缓存，HEAD 从缓存的 GET 返回（保留 Content-Length 等头、body 为空）；
  - `If-None-Match` 对 GET 与 HEAD 都返回 304；
  - 响应 `Vary` 不再整体禁止缓存（`Set-Cookie` 等规则不变）。
- 实测（站点缓存 ttl=60）：
  - gzip：首次 MISS body=enc=gzip，重复 HIT body 一致；
  - br：MISS body=enc=br（未误用 gzip 变体）；无 Accept-Encoding：MISS body=enc=none；
  - HEAD（Accept-Encoding: gzip）：HIT + Content-Length=8，响应无 body；
  - /page 首次 200，带 If-None-Match 重复请求 304；HEAD + If-None-Match 也 304；
  - 指标：hits 5、misses 4、stores 4、entries 4（3 个 /vary 变体 + 1 个 /page）。
- 测试：gateway 51 → 53（新增 Vary 变体隔离、Vary:* 不缓存；现有缓存/代理测试更新签名）；
  dashboard 25、shared 18 + 3；fmt/clippy 通过。
- 限制：仍无 single-flight、`If-Modified-Since`、stale-while-revalidate、持久化；配置同步仍清空全部缓存。
- 文档：`gateway.md` 5.4、`deployment.md` 限制表、`issue.md` P-05/B-09/F-60。

### 统计准确性 + 面板时区（分支 `feat/stats-accuracy`）
- `total_ips`：`MINUTE_ROLLUP` 在秒→分归并时、`DAILY_ROLLUP` 在跨天归并时，对本次归并窗口
  从原始访问日志重新计算 `COUNT(DISTINCT remote_addr)`；只有原始日志已过保留期的缺口
  才回退为秒/分聚合值。`BULK_MINUTE_ROLLUP` 本来就直接从原始日志按分钟去重。
- 保留期配置化：新增 `[stats].raw_retention_days` / `STATS_RAW_RETENTION_DAYS`
  （7..90 天，默认 7，低于 7 会 WARN 并夹到 7）；分区 DROP、边界 DELETE 和日志清理都使用该值。
- 面板时区：`GET /api/summary?tz_offset_minutes=<分钟>`（-840..840），按浏览器时区从原始
  访问日志计算“今日”请求/响应/4xx/5xx/字节数，响应回显 `timezone_offset_minutes`；
  前端 `api.summary()` 自动发送 `-new Date().getTimezoneOffset()`。
- 实测（beta 库）：
  - 插入“本地昨天 UTC、+08 今天”的 1 请求/1 响应(503)/12、34 字节：
    `tz_offset_minutes=480` → requests=1、responses=1、e5xx=1、sizes 12/34；
    `tz_offset_minutes=0` → 全部 0；5000 夹到 840；非数字返回 400；
  - `STATS_RAW_RETENTION_DAYS=3` → 启动 WARN 夹到 7，rollup 正常；
  - 统计集成测试新增断言：同一分钟 2 个不同 IP（3 次请求）→ minute total_ips=2；
    同一天跨两个分钟的 2 个不同 IP → daily total_ips=2。
- 测试：shared 18 → 19（+保留期 clamp）、dashboard 25 → 26（+时区 clamp）、gateway 53；
  3 个集成测试通过；fmt/clippy 通过。
- 仍未做：请求/响应 sizes 仍依赖 Content-Length（chunked 为 0/NULL）；秒/分/日表保留期
  （1h/1d/长期）仍硬编码；边界分区仍逐行 DELETE。
- 文档：`database.md` 5.4、`deployment.md` 环境变量/限制表、`issue.md` P-07/B-08/B-14/F-61、
  `dashboard/backend/config.example.toml`。
### 生产就绪收尾（2026-11-20）
- 6 个批次的最终分支栈（每批均含测试、文档、提交、CI）：
  `feat/dashboard-session-security`（1d0ba33/005bfff）→
  `feat/production-deploy`（ac74a83/340d3c4/27b92f6）→ `feat/observability`（a54eb85）→
  `feat/rate-limit`（ad4196f）→ `feat/health-checks`（9fb72f1）→ `feat/h3-streaming`（30ac6b0）→
  `feat/cache-vary`（61d6c0b）→ `feat/stats-accuracy`（4a0450b）。
- 最终验证：gateway 53、dashboard 26、shared 19 + 3 集成；fmt/clippy 通过；
  前端 format/lint/type-check/build 通过；四个 head CI 全绿；
  叠加 smoke（加密证书 + h3 流式 + 缓存 + 健康检查 + 面板静态托管 + 指标 + 双端 SIGTERM）通过。
- 生产清单：`deployment.md` 第 9/12 节、`issue.md` 第 7 节。
- 生产清单：`deployment.md` 第 9/12 节、`issue.md` 第 7 节。

### 仓库主线整理（2026-11-20）
- 旧 `master`（原型 7ee8c9a）重命名为 `v1`；在原 `v2` 提交（c34922e）上建立新的 `master`，
  并快进合并全部 31 个 feature 分支到 `de1da22`；GitHub 默认分支为 `master`。
- 删除远端/本地全部已合并 feature 分支；远端只保留 `master` 与 `v1`。
- 备注：GitHub 的 `v2 → master` 重命名 API 返回 `New branch already exists`，改用
  “在 v2 提交创建 master → 切换默认分支 → 删除 v2”的等价流程。
- 决策记录：WAF/CC 暂缓；告警保留本地抓取；DNS provider 凭据由面板设置（仍受
  `SECRET_ENCRYPTION_KEY` 保护）；chunked sizes 实际字节统计列为下一编码批次。
### mnt 运维 CLI + Docker 打包（2026-11-20）
- 新增 `mnt/` crate（`webgateway-mnt`）：通过 dashboard 的 Unix socket 获取管理员 TOTP；
  默认输出当前 6 位验证码，`--secret` 输出 secret 与 otpauth URI（首次绑定用）。
- dashboard 新增 `mnt` socket 服务：默认 `/run/webgateway/mnt.sock`，可用 `MNT_SOCKET` 覆盖，
  权限 0600；socket 随进程退出清理。验证：socket mode 600、mnt code 登录 200、`--secret` 与 env 一致。
- 仓库根目录新增 `Dockerfile`、`docker-compose.yml`、`docker/entrypoint.sh`、`docker.env.example`、
  `.dockerignore`：多阶段构建 dashboard/gateway/mnt 与面板；compose 含 dashboard、gateway、mnt-init、
  mnt（tools profile），数据库使用外部 PostgreSQL。
- 文档：`deployment.md` 第 13/14 节、`issue.md` F-62。

