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
