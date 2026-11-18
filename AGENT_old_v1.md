# WebGateway 项目实现总结（来自 old_project）

> 本文档基于 `old_project/webgateway/` 当前工作树整理，用于给后续在本仓库重建/演进 WebGateway 时提供完整背景。
>
> 代码结构、启动路径和已落地的业务逻辑以源码为准；旧项目中的 `agent.md`、`agent-context.md`、`latest-issue.md` 有部分内容停留在“v1→v2 后台数据搬迁”阶段，当前实现已经改为**schema 版本门控、丢弃旧访问日志并重建 v2 空表**，详见第 6 节。
>
> 整理时源码状态：`master` 最后提交 `7ee8c9a`，并包含未提交的第十二轮访问日志 schema 版本门控改动。当前根目录工程仍是骨架，`old_project/webgateway` 是完整实现参考。

## 1. 项目定位

WebGateway 是一个自建的反向代理 / 网关系统，由数据面网关和控制面面板组成：

- **数据面 `gateway`**：基于 Hyper + Tokio + rustls 的 HTTP(S) 反向代理。按 TLS SNI / Host 找到站点，将请求转发到站点上游；支持证书热加载、WebSocket 升级、上游连接池、访问日志采集。
- **控制面 `dashboard-backend`**：基于 axum 的 REST API，负责站点、证书、DNS 提供商、访问统计、系统日志、用户/TOTP/角色管理和 ACME 证书自动签发。
- **控制面 `dashboard-frontend`**：Vue 3 + TypeScript 管理面板，包含登录、统计图表、站点卡片、证书/DNS 管理、设置页。
- **数据库 `PostgreSQL`**：控制面配置和所有访问日志都落在这里；配置表变更通过 `LISTEN/NOTIFY` 通知网关热同步。
- **运维通道 `mnt`**：通过 Unix Socket 提供受限运维命令，当前唯一实现是读取首个管理员的 TOTP 验证码。

```text
Client
  │  HTTP / HTTPS / WebSocket
  ▼
┌──────────────────────────────┐
│ gateway :80/:443 + 站点端口   │
│  SNI/Host 路由 → upstream     │
└──────────────┬───────────────┘
               │ 配置同步（NOTIFY + 10s 兜底）
               ▼
┌──────────────────────────────┐       ┌─────────────────────────┐
│ PostgreSQL                   │◄──────│ dashboard-backend :3000 │
│ 配置表 + 访问日志 + 日汇总    │       │  REST + JWT/TOTP/ACME   │
└──────────────────────────────┘       └────────────┬────────────┘
                                                    │ /api
                                                    ▼
                                         dashboard-frontend :4173
```

## 2. 技术栈

| 维度 | 实现 |
|---|---|
| Rust | workspace resolver 3，多个 crate 使用 edition 2024 |
| 异步运行时 | Tokio |
| 服务端/代理 | Hyper 1.x、hyper-util、tokio-rustls、rustls |
| 控制面 API | axum 0.8 |
| 数据库 | PostgreSQL；sqlx 0.8，启用 `chrono`、`postgres`、`runtime-tokio` |
| PG 扩展 | `uint128`、`btree_gin`（迁移时 `CREATE EXTENSION IF NOT EXISTS`） |
| 配置序列化 | `serde` + `toml` + JSONB |
| 认证 | 每用户独立 JWT secret；`jsonwebtoken`；`totp-rs`（SHA1 / 6 位 / 30 秒） |
| ACME | `acmex`，ZeroSSL，DNS-01，腾讯云 DNS Provider |
| 前端 | Vue 3.5、TypeScript、Vite、vue-router、ky、ECharts、vue-i18n |
| 前端包管理 | pnpm 12.8.1，`pnpm-lock.yaml` 为唯一锁文件 |
| 部署 | Docker Compose；GitHub Actions 构建并推送 GHCR 镜像 |
| 内部 ID | 自定义 12 字节 BSON 风格 `ObjectId`，以 24 位十六进制字符串存 PostgreSQL TEXT |

根 `Cargo.toml` 用本地 `vendor/acmex` 替换 crates.io 的 `acmex`，原因是上游 0.8.0 硬编码了 `aws-lc-rs` 的 FIPS feature，导致部分环境 release 构建失败。

## 3. 目录与模块职责

| 路径 | 职责 |
|---|---|
| `gateway/` | 数据面入口；监听站点端口、TLS、HTTP 代理、访问日志 |
| `gateway/src/upstream.rs` | 当前生效的代理主链路：站点匹配、普通请求转发、连接池重试、WebSocket 升级 |
| `gateway/src/upstream/connection.rs` | 上游连接、DNS 多地址轮询、信号量限流、空闲连接池、可复用判定 |
| `gateway/src/upstream/protocols.rs` | TCP 预读、TLS ClientHello/SNI 探测；PROXY protocol 目前是空实现 |
| `gateway/src/sync/websites.rs` | 全量对账站点、Host/通配符匹配、路径条件、动态监听端口 |
| `gateway/src/sync/cert.rs` | 全量对账证书、过期过滤、SNI 精确/通配符/默认证书解析 |
| `gateway/src/access.rs` | 访问日志内存缓冲、定时刷盘、size 按秒聚合、保留期清理、日汇总刷新 |
| `gateway/src/transport.rs` | 包装响应 Body，统计实际传输字节并在读完后置“连接可复用”标志 |
| `gateway/src/state.rs` | 每站点状态 `WebSiteRunner`、每请求 `ClientState` |
| `gateway/src/proxy/` | 已停用的旧代理实现，与 `upstream` 重叠，`main.rs` 中已注释 |
| `crates/shared/` | 数据库初始化/迁移、数据模型、监听器、日志、连接流包装、favicon、保留期模型 |
| `crates/shared/src/database/access_v2.rs` | 访问日志 v2 分区表、周分区函数、schema 版本门控、日汇总表与刷新 |
| `crates/shared/src/database/dashboard_schema.rs` | `users` / `users_client_secrets` / `web_log` / `users_info` 的共享 DDL |
| `crates/protocols/` | TLS ClientHello 解析、SNI 提取、PROXY protocol v1/v2 解析器 |
| `crates/dnsprovider/` | DNS 服务商抽象；目前只有腾讯云 DNSPod 实现 |
| `crates/acme/` | 半成品 ACME/证书构造实验代码，非当前签发主流程；含硬编码本机路径 |
| `crates/simple_shared/` | 低依赖共享：`ObjectId`、mnt 协议、ZigZag varint、版本信息 |
| `crates/utils/` | hex/脱敏/backlog 等工具 |
| `dashboard/backend/` | 控制面 API、认证授权、证书调度、IP 归属、mnt Unix Socket |
| `dashboard/backend/src/router/` | REST 路由：`/websites`、`/certificates`、`/dnsproviders`、`/logs`、`/access`、`/settings` |
| `dashboard/frontend/` | Vue 管理面板 |
| `mnt/` | 运维 CLI 客户端 |
| `ipdb/` | Python 工具：下载 GeoLite2 / 中国 IP 库 / qqwry，生成 `dashboard/backend/assets/ipdb/` |
| `assets/error_pages/` | 一个 Vite 静态错误页项目；从网关代码看当前未接入 |
| `dev/` | 本地启动脚本、生产升级脚本、前端排版 mock API |
| `tests/` | 运行期证书/日志等产物，不是 Rust 测试目录，且被 `.gitignore` 忽略 |

## 4. 部署拓扑与启动

### 4.1 Docker Compose

`docker-compose.yml` 定义 5 个服务，固定 bridge 网络：

| 容器 | 网络地址 | 端口 | 说明 |
|---|---|---|---|
| `webgateway-pg` | `${SUBNET_PREFIX}.2` | 仅容器网络 | PostgreSQL 数据目录 bind mount 到 `${DATA_DIR}/pg` |
| `webgateway-gateway` | `${SUBNET_PREFIX}.3` | `80:80`、`443:443` | 数据面 |
| `webgateway-dashboard-backend` | `${SUBNET_PREFIX}.4` | 容器内 3000 | 控制面 API + mnt + ACME 调度器 |
| `webgateway-dashboard-frontend` | `${SUBNET_PREFIX}.5` | `7173:4173` | nginx 静态站点 + `/api/` 反代 |
| 无 | | | gateway 还会按 DB 中站点的 `ports` 动态监听额外端口 |

前端镜像的 `entrypoint.sh` 用 `envsubst` 把 `BACKEND_URL` 注入 nginx 配置；`/api/` 反代到后端时会去掉 `/api` 前缀。Compose 中 PostgreSQL 没有把 5432 发布到宿主机。

### 4.2 数据库启动模式

`shared::database::DbStartupMode` 支持三种模式：

| 触发方式 | 行为 |
|---|---|
| 默认（未设置 `DB_AUTO_MIGRATE`） | `AutoMigrate`：先迁移再服务 |
| `DB_AUTO_MIGRATE=0` 或 `false` | `Serve`：只校验 schema，不执行 DDL |
| 任意服务进程加 `--migrate` | `Migrate`：执行迁移后退出，供一次性 Job 使用 |

关键实现：

- gateway 和 dashboard 共用同一迁移入口；默认都会自动迁移，因此谁先启动都能得到完整 schema。
- 迁移在 `locks::SCHEMA_INIT` 事务级 advisory lock 下串行执行；`SET LOCAL lock_timeout = '60s'`，避免一个慢迁移让另一个进程无限挂起。
- `CREATE EXTENSION` 不能放在显式事务里，因此在锁外单独执行；其它 DDL 在同一事务中。
- 迁移末尾会 `verify_database_schema()`，校验必需表与关键列；缺失会明确报错而不是带病启动。
- 必需表包括：`configurations`、四张访问日志表、`certificates`、`dns_providers`、`websites`、`users`、`users_client_secrets`、`web_log`、`access_stats_daily`。
- 关键列校验包括 `users.role`、`certificates.signing_started_at`、`certificates.expires_at`、`access_request_logs.requested_at`、`access_request_logs.remote_addr`。

### 4.3 配置优先级

两个后端都先尝试读取**当前工作目录下的 `config.toml`**，不存在或解析失败则回退到环境变量和默认值。

| 配置 | 环境变量 | 默认 |
|---|---|---|
| 数据库连接 | `DATABASE_URL` | 空，未设置会告警 |
| 连接池大小 | `DATABASE_MAX_CONNECTIONS` | `10` |
| dashboard API 端口 | `DASHBOARD_API_PORT` | `3000` |
| JWT 有效期 | `TOKEN_EXPIRES` 秒 | 7 天 |
| 迁移开关 | `DB_AUTO_MIGRATE` | 自动迁移 |
| 统计汇总刷新 | `ACCESS_STATS_ROLLUP=off/0/false` 可关闭 | 开启 |
| mnt socket | `MNT_SOCKET` | `/tmp/webgateway-mnt.sock` |
| 前端后端地址 | `BACKEND_URL` | `http://localhost:3000` |

### 4.4 常用命令

```bash
# Rust 全量检查
cargo check --workspace --all-targets
cargo test -p shared -p gateway

# 构建发布二进制
cargo build --release --bin dashboard --bin gateway --bin webgateway-mnt

# 前端
cd dashboard/frontend
pnpm install --frozen-lockfile
pnpm build
pnpm typecheck
pnpm lint

# 本地启动（dev/environment 提供 DATABASE_URL）
./dev/gateway
./dev/dashboard_backend
./dev/dashboard_frontend
./dev/mnt
```

注意：`dev/dashboard_frontend` 仍写着 `yarn vite dev`，与当前 pnpm 迁移后的工程不一致；需要本地跑前端时直接用 `pnpm dev` 或 `pnpm exec vite`。

## 5. 数据面 Gateway 实现细节

### 5.1 启动与动态监听

`gateway/src/main.rs` 的顺序：

1. 初始化日志、读取配置。
2. 初始化数据库并执行/校验迁移。
3. 用 `--migrate` 时直接退出。
4. 启动访问日志刷盘后台任务。
5. 启动每小时清理/分区维护/日汇总刷新任务。
6. 首次全量同步站点与证书。
7. 等待 Ctrl-C。

监听器由 `sync::sync_websites()` 返回的端口集合驱动：

- `sync_listeners(desired)` 做对账式 diff：新端口 bind 后加入 `LISTENERS`，不再需要的端口 abort 对应 task。
- 先 bind 成功再登记监听，避免 bind 失败却误以为已监听。
- 双栈监听器 `CustomDualStackTcpListener` 同时绑定 IPv4 0.0.0.0 和 IPv6 `::`，开启 `SO_REUSEADDR` / `SO_REUSEPORT`。
- accept 遇到 fd 耗尽会退避；连接建立后每个连接独立 `tokio::spawn`。

### 5.2 TLS / SNI / 证书热加载

每个新 TCP 连接先做预读：

- `get_proxy_protocol()` 当前直接返回 `None`，PROXY protocol 尚未接入主链路。
- `get_tls_sni()` 预读 TLS record / ClientHello，整体超时 10 秒。
- ClientHello 长度硬上限 64 KiB，防止恶意长度导致大内存分配。
- 如果首字节是 TLS handshake，则用 `TLS_ACCEPTOR` 接受 TLS；TLS 证书由 `AutoCertificate` 从内存证书表解析。
- `SERVER_CONFIG` 的 ALPN 顺序是 `h2`、`http/1.1`，因此客户端侧可协商 HTTP/2。

证书同步规则：

- `sync_certificates()` 全量读取 DB 证书，跳过 fullchain/private key 缺失或解析失败的证书，避免一张坏证书阻断全部证书加载。
- 证书过期规则：只要存在未过期证书，就**只装载未过期证书**；全部过期时才回退装载全部，保证仍然可握手。
- 精确域名和通配符域名分别存表；通配符会被转成预编译正则（`*` → `[-\w]+`），按模式长度降序匹配，更具体优先。
- 证书解析有 2 小时 TTL 缓存；每次全量同步会清空，避免已删除证书继续命中。
- 默认自签名证书由 `sign_default_certificates()` 生成，在没有 SNI 或 SNI 无匹配证书时兜底。
- `websites.certificates` 字段目前只是站点配置数据，网关实际证书选择按证书自身的 `hostnames` 全局匹配，并不按站点关联过滤。

### 5.3 站点同步与路由

`sync_websites()` 每轮全量从 DB 读取站点并重建内存表：

- `WEBSITES`：`ObjectId → WebSiteRunner`。
- `FULL_WEBSITES`：精确 Host → 站点。
- `LAZY_WEBSITES`：通配符模式 → `(Regex, 站点)`。
- `CACHE_WEBSITES`：Host → 站点，2 小时 TTL。

同步是全量对账而不是增量：先构建新集合，再清空旧表重建。这样删除站点、删除域名、修改端口都会生效。网关同时监听 PostgreSQL 的 `websites_updater` / `certificates_updater` 频道，并保留 **10 秒一次全量兜底同步**，防止 NOTIFY 丢失导致永久使用旧配置。

请求匹配时：

1. 从 HTTP `Host` 头取主机名；如果 URI 带 authority，优先 URI authority。
2. 用 `strip_port()` 去掉端口，支持域名、IPv4、IPv6 字面量。
3. 精确匹配优先，再查缓存，再按通配符长度降序匹配。
4. 额外要求“路径能匹配该站点的任一 backend 的 `match_path`”，否则不算命中。
5. 没有站点的请求返回 404，并仍然记访问日志。

注意：`match_path` 当前只决定**站点是否命中**；命中后 `WebSiteRunner` 永远只取 `backends.first()`，不会根据路径切换到不同 backend；`main`、`balance` 字段也未参与负载均衡。

### 5.4 请求转发链路

`ConnectionCycle::handle_connection()` 完成预读和 TLS 后，用 hyper auto builder 处理 HTTP/1.1 或 HTTP/2 请求并支持 upgrades。每个请求：

1. 生成新的 `ObjectId` 作为 request id。
2. 抓取 Host、path、headers、method、version、body size hint、远端 IP。
3. `get_website()` 匹配站点；匹配不到时记录请求日志并返回 404。
4. 对普通 HTTP 请求：
   - 只向头端添加/覆盖 `Host`、`X-Real-Ip`、`X-Forwarded-For`、`X-Forwarded-Proto`、`X-Forwarded-Host`。
   - 当前 `X-Forwarded-For` 是覆盖为 socket 远端 IP，不保留代理链；`website.config.get_request_ip` 也尚未在数据面实现。
   - 转发路径保留 backend base path 前缀；query string 原样拼接。
   - 客户端侧请求版本会保留到响应；上游连接目前固定使用 HTTP/1.1。
5. 普通请求整体有 60 秒超时；没有后端匹配或后端出错时返回：
   - 404：`Not Found`
   - 500：`Gateway error`，并附 `Detail-Error`
   - 408：`Request Timeout`
   - 400：`Bad Request`
   - 所有响应补 `Server: WebGateway`。
6. 响应体通过 `StatisticsIncoming` 流式返回，同时累计真实传输字节；读到 EOF 时置连接可复用标志。
7. 请求日志进内存；响应日志在响应头生成后进内存。成功访问后端时 `backend_responsed_at` 取数据库时间，其余状态为空。

WebSocket：

- 检测到 `Upgrade: websocket` 后走单独路径。
- 不做连接池复用，直接新建上游 TCP。
- 与上游完成 HTTP/1.1 握手；如果后端返回 101，则复制 `upgrade` / `connection` / `sec-websocket-*` 头并把 `OnUpgrade` 桥接给客户端。
- 两个 upgrade future 成功后用 `copy_bidirectional` 双向转发。
- 上游连接 future 被 120 秒超时包裹，因此超长 WebSocket 会被截断；客户端连接本身还有 300 秒总生命周期限制。

### 5.5 上游连接池

`WebSiteRunner::new()`：

- 取站点的第一个 backend URL。
- 同步阶段做 DNS 解析，把主机名解析成 `Vec<SocketAddr>` 存到连接池。
- 保留 backend URL，用于 base path 拼接和 Host 语义。

`UpstreamConnectionPool`：

- 每个站点一个池。
- 默认上限 `DEFAULT_MAX_CONNECTIONS = 256`；用 `Semaphore` 控制并发许可，取许可超时 5 秒后快速返回 502。
- 空闲连接队列最多 64 条；超出直接关闭。
- 新连接在 DNS 解析出的多个地址间轮询尝试。
- 取连接时先释放队列锁，再在锁外做 50ms 健康检查；健康检查用零长度 `read` 探 EOF，不再使用无效的 `write(&[])`。
- 只有响应体读到 EOF **且**连接驱动任务仍未退出时，连接才允许回空闲池；否则关闭，防止响应串包和“投递到无接收者队列导致永久挂起”。
- 复用连接发送时如果 hyper 返回请求体被原样交还（一个字节都没发出），会换新连接安全重试一次；新连接失败不重试。
- hyper 连接驱动任务最多维持 120 秒；这导致空闲复用率偏低，但并发上限和背压是明确存在的。

已知限制：站点配置里的 `https://` backend 目前不会启用上游 TLS，因为 `WebSiteRunner` 构造池时没有设置 `tls_config`，连接仍走裸 TCP；上游也固定 HTTP/1.1，没有 HTTP/2 客户端。

## 6. 访问日志、分区与统计

### 6.1 写侧缓冲与刷盘顺序

访问日志生产者运行在请求热路径上，只写内存，不做数据库 IO：

- 请求主日志：`ACCESS_REQUEST_LOGS`，`PendingBuffer<AccessCreateRequest>`。
- 响应主日志：`ACCESS_RESPONSE_LOGS`，`PendingBuffer<AccessCreateResponse>`。
- 请求/响应大小更新：`SizeBuffer`，按 id 保留最新累计值。
- 响应大小插入：`SizeAccumulator`，按 `(id, 秒)` 累加。
- 请求大小插入：`PendingBuffer<AccessInsertRequestSize>`。

`PendingBuffer` 有 `pending` 和 `inflight` 两个槽位。刷盘规则是：

1. 从 pending 取出一批到 inflight。
2. 写库成功后清空 inflight。
3. 写库失败则批次留在 inflight，下一轮优先重试。
4. 生产者持续向 pending 追加，因此 DB 短暂不可用不会丢日志，只会造成内存积压。

其它细节：

- 刷盘循环对齐数据库时间到整秒，最小间隔 1 秒；单轮超过 3 秒告警，积压超过 20 万条告警。
- 主表每批最多 1000 行，之后的 size 任务并发执行。
- 主表 INSERT 必须幂等：批量插入按参数上限分块，但没有跨块事务，所以使用无冲突目标的 `ON CONFLICT DO NOTHING`，同时兼容 v1 主键和 v2 分区主键。
- 请求主日志、响应主日志必须先于 size 更新/插入写库，否则 size 表的父行守卫会跳过。
- size 插入使用「DELETE 旧行 RETURNING 旧值 → INSERT 旧值 + 本次增量」，整批单事务。这样可以同时兼容全量唯一索引与部分唯一索引，并且不会因为分块提交后重试而重复累加。
- 每次累加前还会 `WHERE EXISTS` 检查父日志行存在；父行缺失时静默跳过，避免一个坏 row 永久卡死整批刷盘。
- 同一批次内同 `(id, 秒)` 会先在内存合并，避免对同一键执行多次累加。
- 请求/响应 body 大小取 `SizeHint.lower()` 并限制到 `u32::MAX`；响应实际传输字节由 `StatisticsIncoming` 逐 chunk 记录。

### 6.2 访问日志 v2 结构

访问日志 schema 当前版本为 `2`，版本号写在：

```text
configurations.key = access_logs_schema_version
configurations.value = {"data": 2}
```

四张表都是 PostgreSQL 声明式 RANGE 分区表：

| 表 | 分区键 | 主键 | 说明 |
|---|---|---|---|
| `access_request_logs` | `requested_at` | `(id, requested_at)` | 请求主日志 |
| `access_response_logs` | `responsed_at` | `(id, responsed_at)` | 响应主日志 |
| `access_request_size_logs` | `at_second` | `(id, at_second)` | 请求体按 `(request_id, 秒)` 唯一 |
| `access_response_size_logs` | `at_second` | `(id, at_second)` | 响应体按 `(response_id, 秒)` 唯一 |

其它约定：

- 分区名格式：`<父表逻辑名>_<IYYY>_w<IW>`，例如 `access_request_logs_2026_w40`。
- `wg_ensure_weekly_partition(parent, week_start)` 建单周分区，内部用 advisory xact lock 防并发重复建。
- `wg_ensure_upcoming_weeks(2)` 预建上一周、当前周和未来两周。
- `wg_drop_partitions_older_than(days)` 只删“整周都早于截止日”的分区，并在数据库函数内强制 `days >= 90`。
- 网关每小时调用一次分区预建和过期分区回收；v2 去掉了日志表之间的外键，引用完整性由应用层父行守卫保证。
- 旧视图 `qps_per_second`、`qps_per_5s`、`daily_traffic_by_website` 已不再创建；QPS 查询直接按时间列过滤以利用分区裁剪。

### 6.3 Schema 版本门控（重要，会删历史数据）

`ensure_access_logs_schema(tx)` 的规则：

- 如果 `access_logs_schema_version == 2` 且四张逻辑表都已经是分区表：
  - 幂等重建缺失的表/索引/函数/分区；
  - 确保日汇总表存在；
  - 不删除数据。
- 否则（版本缺失、旧版本、或四张表不全为分区表）：
  - `DROP TABLE ... CASCADE` 删除所有访问日志相关对象：四张逻辑表、`*_v1`、`access_v2_*`、`*_p`、`access_stats_daily`、旧迁移进度表等；
  - 删除旧 QPS/流量视图；
  - 重建全新的 v2 空表；
  - 写入版本号 2。

因此，部署到老库时，下一次启动迁移会**自动丢弃全部历史访问日志并重建空表**。这是用户决策后的预期行为；部署前若需保留历史数据，必须自行导出。此行为与旧文档中的“后台搬迁、切换、核对、回收”完全不同。

### 6.4 日汇总表与面板查询

`access_stats_daily` 按 `(day, website_id)` 预计算：

- `total_requests`、`total_responses`、`total_ips`、`e4xx_requests`、`e5xx_requests`、`backend_error_requests`、`total_requests_size`、`total_response_size`。
- 没有站点归属的请求归入 `website_id = "__unknown__"`。
- 网关每小时刷新最近 3 天，并在首次运行时逐步回填最近 30 天；`ACCESS_STATS_ROLLUP=off` 可临时关闭。
- 面板的“近 N 天概览”和站点卡片“今日指标”走该汇总表；QPS 和访问地图 IP 聚合仍实时扫描原始日志。
- `total_ips` 是每日独立 IP 数求和，跨天同一 IP 会重复计入；不是整个窗口的精确去重。

### 6.5 保留期清理

配置存于 `configurations.key = access_log_retention`：

```json
{"data": {"retention_days": 180, "enabled": true}}
```

- 默认 180 天；允许范围 90~3650 天；超界会夹紧后返回实际生效值。
- 网关每小时执行一次。
- 使用会话级 `pg_try_advisory_lock(locks::RETENTION)`；必须在同一条连接上取锁、删除、解锁，否则锁会静默失效。
- 每轮最多 5 万行，最多 20 轮；单次面板“立即清理”最多 5 轮 × 5 万行。
- 删除顺序：响应 size → 请求 size → 响应主表 → 请求主表；v2 虽无外键，仍保持该顺序。
- v2 下优先按 `at_second`/时间列分区裁剪；整周过期分区还会被 `wg_drop_partitions_older_than` 直接 DROP。

## 7. 控制面 Dashboard 实现

### 7.1 启动与认证

启动流程：

1. 初始化日志和配置。
2. 初始化数据库（默认自动迁移）。
3. `ensure_default_admin()`：如果没有任何用户，创建默认管理员；用户名取环境变量 `USERNAME`，否则 `admin`；随机生成 TOTP secret 和 JWT secret；角色 `admin`。
4. 启动 axum API、mnt Unix Socket 监听、证书调度器。
5. 等待 Ctrl-C，退出时 abort 后台任务。

认证是 **TOTP-only 登录，没有密码登录**：

- `POST /auth/login` 请求体 `{ username, totp }`。
- TOTP 算法 SHA1、6 位、30 秒步长，issuer 为 `WebGateway`。
- 登录成功用该用户独立的 `jwt_secret` 签发 JWT，claims 为 `id`、`iat`、`exp`；默认 7 天。
- JWT 校验流程：先不验签解出 `id`，按 id 从库取用户，再用该用户的 `jwt_secret` 验签。
- 资源路由统一挂 `middle_refresh_token`：有效请求成功返回 200 时，在响应头写新的 `Refresh-Token` 和 `Refresh-Token-Expired`。
- 登录失败会随机延迟 2~5 秒；没有账号锁定或速率限制实现。
- TOTP 绑定流程：已登录用户先用当前 TOTP 获取二维码；临时 secret 存进程内 TTL 缓存（10 分钟）；验证绑定 TOTP 后写入 `users_client_secrets`。进程重启会丢失未完成的绑定会话。
- `users_client_secrets` 支持一个用户绑定多个 TOTP；登录时主 secret 和扩展 secret 都会尝试。
- 数据库中 `totp_secret` / `jwt_secret` 是明文存储。

权限模型：

| 角色 | 只读 | 创建/修改 | 删除/账号管理 |
|---|---:|---:|---:|
| `view` | ✅ | ❌ | ❌ |
| `user` | ✅ | ✅ | ❌ |
| `admin` | ✅ | ✅ | ✅ |

- 无法识别的角色降级为 `view`。
- `admin` 不能通过接口修改自己的角色，防止误操作锁死管理员。
- 需要写权限的 handler 显式调用 `Authorizer::require_write()`；删除站点、账号枚举、改角色调用 `require_admin()`。

### 7.2 REST API

统一响应体：

```json
{"status": 200, "message": "可选", "data": {}}
```

HTTP 状态码与 body 中的 `status` 一致。全局中间件提供：

- `CatchPanicLayer` 捕获 panic。
- 客户端 IP 提取：按 Cloudflare、CloudFront、Fly、True-Client-IP、X-Real-IP、最右 X-Forwarded-For 的优先级读取，最后回退 socket 地址。
- 请求日志。
- 当前没有 CORS 中间件，生产通过同源 nginx `/api` 代理访问。

| 方法 | 路径 | 权限 | 说明 |
|---|---|---|---|
| POST | `/auth/login` | 公开 | 用户名 + TOTP 登录 |
| GET | `/auth` | 登录 | 当前用户信息 |
| GET | `/auth/info?user_id=` | 自己或 admin | 查询用户信息 |
| GET | `/auth/refresh` | 登录 | 重新签发 JWT |
| POST | `/auth/totp/qrcode/get` | 登录 | 验证当前 TOTP 后生成绑定二维码 |
| POST | `/auth/totp/qrcode/refresh` | 登录 | 刷新绑定二维码 |
| POST | `/auth/totp/qrcode/verify` | 登录 | 验证并保存新的 TOTP secret |
| GET | `/auth/users` | admin | 枚举用户 |
| POST | `/auth/users/role` | admin | 修改用户角色 |
| GET | `/websites/` | 任意角色 | 站点列表 |
| POST | `/websites/create` | user+ | 创建站点 |
| GET | `/websites/{id}` | 任意角色 | 单个站点 |
| POST | `/websites/{id}/update` | user+ | 整条覆盖更新，0 行返回 404 |
| POST | `/websites/{id}/delete` | admin | 删除站点 |
| GET | `/certificates/total` | 任意角色 | 证书总数 |
| GET | `/certificates/page?page=&limit=` | 任意角色 | 证书分页；返回前清除私钥/fullchain |
| POST | `/certificates/create` | user+ | 手动上传或 AUTO 申请证书 |
| GET | `/dnsproviders/total` | 任意角色 | DNS 提供商总数 |
| GET | `/dnsproviders/page?page=&limit=` | 任意角色 | 分页；清除凭据 |
| POST | `/dnsproviders/create` | user+ | 创建 DNS 提供商 |
| GET | `/logs/total` | 任意角色 | 系统日志总数 |
| GET | `/logs/page?page=&limit=` | 任意角色 | 系统日志分页 |
| GET | `/access/qps?interval=1\|5&count=` | 任意角色 | QPS；count 限制 1..=300 |
| GET | `/access/info?in_days=1\|7\|30` | 任意角色 | 汇总指标 |
| GET | `/access/metrics/websites` | 任意角色 | 今日站点指标 |
| GET | `/access/access_map?in_days=&type=global\|china` | 任意角色 | IP 归属聚合地图 |
| GET | `/settings/retention` | 任意角色 | 读取保留期 |
| POST | `/settings/retention` | user+ | 更新保留期 |
| POST | `/settings/retention/prune` | user+ | 立即清理一批 |

注意：前端证书/DNS 分页请求当前传的是 `size`，后端模型读取的是 `limit`；这会导致后端回退默认 20 条，后续统一 API 时应修正。

### 7.3 证书签发

`dashboard/backend/src/certificate.rs` 是当前 ACS 主流程：

- 后台调度器每 30 秒跑一次。
- 单轮最多认领 4 张证书。
- 待签条件：`expires_at IS NULL OR expires_at < NOW() + 7 days`，且 `dns_provider_id` 和 `email` 都非空。
- 用 `FOR UPDATE SKIP LOCKED` 抢占式认领，多实例不会重复签发。
- `signing_started_at` 超过 10 分钟视为陈旧认领，可重新认领，覆盖进程被 SIGKILL 等没有执行 Drop 的情况。
- `SigningGuard` 在正常返回、提前 return、panic 展开时都释放认领标记。
- ACME 使用 ZeroSSL Directory `https://acme.zerossl.com/v2/DV90`，DNS-01 challenge；DNS provider 目前只支持腾讯云 DNSPod。
- 签发成功后将 fullchain/private key 写回 `certificates` 并更新 expires_at。
- `PENDINGS` 是进程内任务表，仅用于本进程观察；跨进程互斥依赖数据库行锁和 `signing_started_at`。
- 证书列表 API 会调用 `remove_sensitive_info()`，将 fullchain/private_key 置空、email 脱敏。
- 手动上传证书会解析 leaf 证书的 SAN 和 not_after，并校验公钥与私钥匹配。

### 7.4 IP 归属与访问地图

`dashboard/backend/src/ip.rs`：

- 优先用 `China_Country.mmdb` 判断；失败或需要城市时再用 `GeoLite2-City.mmdb`。
- 中国 IP 进一步用纯真 `qqwry.ipdb`（`cz88`）补城市。
- `ip2region.rs` 已停用。
- 访问地图对 `access_request_logs` 按 `remote_addr` 实时 `GROUP BY`，然后用 IP 库映射到国家或中国城市；无法解析的 IP 直接跳过。
- 这仍然是全量聚合路径，不走日汇总表，因为 `(day, website_id)` 汇总无法还原 IP 明细。

### 7.5 mnt 运维通道

- 后端启动时监听 Unix Socket，默认 `/tmp/webgateway-mnt.sock`，支持 `MNT_SOCKET` 覆盖；Drop 时自动清理 socket 文件。
- 协议是 JSON 消息 + 长度前缀，长度用 ZigZag varint 编码。
- 当前只有一个请求：`AdminTOTP`。后端取首个用户，生成当前 TOTP，写一条 `mnt.get_totp` 系统日志后返回。
- `mnt` CLI 命令为 `admin-totp`，可指定 `--socket`。Docker 后端镜像把 `webgateway-mnt` 软链到 `/usr/bin/mnt`，所以可在容器内执行 `mnt admin-totp` 获取管理员 TOTP。

## 8. 数据库模型

### 8.1 表与视图

| 对象 | 关键字段/行为 |
|---|---|
| `configurations` | `key TEXT PK`、`value JSONB`；保存保留期和访问日志 schema 版本 |
| `users` | `id TEXT PK`、`username`、`totp_secret`、`jwt_secret`、`role`、`last_login`、`last_ip`、`addresses TEXT[]`；用户名不区分大小写唯一 |
| `users_client_secrets` | `user_id FK`、`secret`；额外绑定的 TOTP |
| `users_info` | 用户信息视图，额外计算 `client_secrets_count`、`bound_totp` |
| `web_log` | 系统/认证操作日志：`user_id`、JSONB `content`、`address` |
| `dns_providers` | `id`、`name`、`provider JSONB`、`domains TEXT[]` |
| `certificates` | `id`、`name`、`hostnames TEXT[]`、`fullchain`、`private_key`、`dns_provider_id FK ON DELETE SET NULL`、`email`、`expires_at`、`signing_started_at` |
| `websites` | `id`、`name`、`hosts TEXT[]`、`ports uint2[]`、`certificates TEXT[]`、`backends JSONB`、`config JSONB` |
| `access_request_logs` | 分区表，按 `requested_at` 周分区 |
| `access_response_logs` | 分区表，按 `responsed_at` 周分区 |
| `access_request_size_logs` | 分区表，按 `at_second` 周分区，唯一 `(request_id, at_second)` |
| `access_response_size_logs` | 分区表，按 `at_second` 周分区，唯一 `(response_id, at_second)` |
| `access_stats_daily` | 日汇总，PK `(day, website_id)` |

索引要点：

- `websites.hosts`、`websites.name` 使用 GIN。
- `certificates.expires_at` 有索引，供续签扫描。
- 访问日志按时间、站点+时间、远端 IP+时间、主键/父 ID 建索引；周分区各自维护。
- `access_stats_daily.day` 有索引。

### 8.2 NOTIFY / updated_at 触发器

`websites`、`certificates`、`dns_providers` 各有两个触发器：

- `AFTER INSERT OR UPDATE` 调 `notify_change()`，发送 `pg_notify('<table>_updater', JSON {id, updated_at})`。
- `BEFORE UPDATE` 调 `update_updated_at()`，用 `clock_timestamp()` 写 `NEW.updated_at`。

使用 `clock_timestamp()` 而不是 `NOW()` 是为了避免长事务提交后更新时间早于同步水位。网关虽然已经改成全量对账，NOTIFY 仍用于快速触发同步。

### 8.3 时间与 ID

- 网关维护数据库时间偏移：每 10 秒采样一次数据库 `NOW()`，取往返中点修正本地时间；访问日志时间与数据库时间同源。
- `ObjectId` 结构：4 字节秒级时间戳 + 5 字节随机值 + 3 字节进程内自增计数器；序列化为 24 位 hex，在 PostgreSQL 中映射为 TEXT。

## 9. 前端面板

入口：`dashboard/frontend/src/main.ts`，挂载 `App.vue` 和全局 Dialog/Presentation 插件。API 基址为同源 `/api`，由 Vite dev proxy 或 nginx 转发。

页面：

- `/login`：用户名 + TOTP 登录。
- `/statistics`：近 24 小时 / 7 天 / 30 天切换；请求次数、独立 IP、错误数/率、流量、QPS 图、访问地图、访问统计。
- `/websites`：站点卡片列表、搜索、今日请求/流量、上游地址、添加/编辑/删除站点。卡片使用 CSS Grid 实现三列自适应布局。
- `/websites/certificates`：证书列表 / 域名解析两 Tab；手动上传/自动申请证书、创建 DNS 提供商。
- `/settings`：控制台用户管理、系统日志、数据保留三个 Tab。

关键前端行为：

- `ky` 全局禁止自动重试（`retry: 0`），超时 15 秒，避免 DB 慢时轮询放大成 3 倍查询。
- `gotWithAuth` 自动从 localStorage 取 JWT 加 `Authorization: Bearer ...`。
- 响应带 `Refresh-Token` / `Refresh-Token-Expired` 时自动更新本地 token。
- `useVisiblePolling`：页面隐藏时暂停轮询，恢复可见立即刷新；连续失败指数退避，最大 5 分钟；卸载时清理定时器和监听器。
- 登录成功后 token 存 `localStorage`；路由守卫在进入受保护页面前用 `/auth` 校验 token。
- 使用 `vite-plugin-svg-icons` 生成 SVG symbol。
- ECharts 使用中国/世界地图 JSON。
- 主题支持 light/dark。

前端工程注意点：

- `main.ts` 还 `import './main.js'`，`main.js` 与 `main.ts` 并存，后续应确认清理。
- 目前没有前端单元测试/组件测试。
- `pnpm lint` 存在存量问题；CI 也没有执行 lint/typecheck。
- 分页接口的 `size` / `limit` 参数名不一致（见第 7.2 节）。

## 10. 测试、构建与 CI

### 10.1 测试

工作区快照约 20 个 Rust 测试：

- `crates/shared`：启动模式默认值、DDL 拆分、访问日志 schema/分区 key、汇总 DDL、保留期夹紧。
- `gateway`：Host 去端口、证书过期装载规则、size 按秒聚合与 rollback。
- `dashboard`：授权链路的真实数据库集成测试；未设置 `DATABASE_URL` 时自动跳过。该测试刻意把所有断言放在一个 `#[tokio::test]` 中，因为 `shared::DATABASE` 是进程级 `OnceLock`，跨 tokio runtime 复用连接池会超时。

没有前端自动化测试。`tests/` 目录不是测试目录，而是运行期证书、启动日志等产物。

### 10.2 CI

GitHub Actions 只在 `master` push、`v*` tag 和手动触发时：

1. 构建 Rust `dashboard`、`gateway`、`webgateway-mnt`。
2. 下载 GeoIP 数据库。
3. 准备 Docker build context。
4. 构建并推送 `gateway`、`dashboard-backend`、`dashboard-frontend` 镜像到 GHCR。

当前 CI **没有** `cargo fmt --check`、`cargo clippy`、`cargo test`、前端 lint/typecheck 门禁。

### 10.3 生产升级脚本

`dev/upgrade.sh` 的意图是：

- 检查 compose 和 `.env`。
- 用 `pg_dump -Fc` 做容器内备份并复制到宿主机 `backups/`。
- 停止并删除容器（数据在 bind mount 里保留）。
- 拉取新镜像并 `docker compose up -d`。
- 校验容器数、宿主机 80 端口、v2 分区表/汇总表、网关启动日志。

脚本不主动搬数据，不做 schema 变更；实际迁移由服务启动时的 AutoMigrate 完成。由于当前版本门控会丢弃旧访问日志，升级前必须明确这是预期行为并做好导出/备份。

## 11. 已确认的已知限制与风险

以下为从当前源码可直接确认或旧项目文档明确记录的遗留项，重建时应优先决定是否继承：

### 数据面

1. **PROXY protocol 未接入**：`gateway/src/upstream/protocols.rs` 中 `get_proxy_protocol()` 只预读并返回 `None`，真实 PROXY header 无法被消费，也不会改变远端地址。
2. **多后端未实现**：站点只使用 `backends.first()`；`balance` 和 `main` 不参与选择。
3. **路径级后端路由未实现**：`match_path` 只用于判断站点是否命中，不能把不同路径转发到不同 backend。
4. **上游 TLS/HTTP2 未实现**：`https://` backend 实际仍以明文 TCP 连接，且固定 HTTP/1.1。
5. **IP 获取配置未生效**：`website.config.get_request_ip` 有模型和前端字段，但 gateway 未使用；始终使用直接连接 socket 的 IP，并覆盖 X-Forwarded-For。
6. **长连接限制**：客户端连接 300 秒总超时，上游连接任务 120 秒超时；WebSocket/SSE 等超长连接会被截断。
7. **连接复用率偏低**：出于挂起/串包安全，连接驱动任务结束就关闭；同一客户端 keep-alive 内可能复用，但池的空闲复用率不高。
8. **站点同步是强耦合的**：全量同步中任一站点 DNS 解析失败/无 backend 会让本轮同步失败；首次同步失败会导致 gateway 启动失败，需要配置数据保证可解析。
9. **站点证书关联是元数据**：`websites.certificates` 不决定 TLS 证书；证书按自身 hostnames 全局匹配。
10. **死代码**：`gateway/src/proxy/`、空文件 `dns.rs`、`foundation.rs`、`proxy/rules.rs` 尚未清理。

### 控制面/安全

1. **TOTP-only 且 secret 明文**：没有密码；`totp_secret` 和 `jwt_secret` 在 DB 中明文存储，生产依赖数据库访问控制。
2. **客户端 IP 头可伪造**：如果 dashboard API 直接暴露公网，中间件会信任多种代理头；应只允许可信反代访问，或在反代层剥离。
3. **缺少登录速率限制/锁定**：只有失败随机延迟 2~5 秒。
4. **TOTP 绑定临时状态在进程内存**：重启丢失；多副本间不共享。
5. **部分 CRUD 不完整**：证书、DNS 提供商只有创建和分页，没有更新/删除接口；证书申请 TODO 仍存在。
6. **分页参数名不一致**：前端证书/DNS 使用 `size`，后端读 `limit`。
7. **无 CORS 中间件**：依赖同源 nginx；如果拆分前端域名需补充 CORS。
8. **证书 pending 行的 `expires_at` 可为空**：数据库 DDL 允许空，而 `DatabaseCertificate` 模型字段是非 Option；自动申请待签发阶段的数据读取路径需要复核。

### 存储/运维

1. **访问日志 schema 升级是破坏性的**：版本不匹配会 DROP 并重建全部访问日志和汇总表，历史数据不保留。
2. **统计汇总只覆盖最近 30 天回填 + 最近 3 天刷新**：更久数据需要手工触发或扩展；面板近 30 天以外的汇总可能不完整。
3. **访问地图仍实时全量聚合**：大时间窗口/大表下会变慢，且不经过日汇总表。
4. **`__unknown__` 归属桶**：未匹配站点或未带 `website_id` 的日志会被归入 unknown，按站点展示时被过滤，总数统计仍包含。
5. **旧文档与代码不一致**：`agent.md`、`agent-context.md`、`latest-issue.md` 中大量“后台自动迁移/换名/回收 v1”内容已经过时；实际以 `crates/shared/src/database/access_v2.rs` 和 CHANGELOG 第十二轮为准。
6. **`assets/error_pages/` 未接入**：目前只是存在的静态工程，网关错误响应仍是纯文本。
7. **`crates/acme` 是半成品**：有调试 `println!`、未使用字段、硬编码本机绝对路径；不应作为当前证书签发主流程参考。
8. **`assets/error_pages` 仍使用 yarn**，与主前端 pnpm 不一致。

## 12. 后续在本仓库重建时的建议

1. 将 `old_project/webgateway` 当作**行为参考和需求来源**，不要直接假设根目录已有对应实现；当前根目录只有空的 `crates/`、`dashboard/`、`gateway/` 骨架。
2. 优先读源码而不是旧交接文档；涉及访问日志时先确认目标是否仍采用“版本门控丢弃旧数据”的策略，还是重新实现无损迁移。
3. 若保留访问日志模块，必须守住这些不变量：
   - 写侧先写库成功再清内存；
   - 主表插入幂等；
   - size 更新/插入在父行之后；
   - size 累加整批单事务且先合并同秒同键；
   - 保留期解锁必须在同一条连接上。
4. 若继续使用 TOTP-only 认证，至少补齐速率限制、secret 加密/受控访问、首个管理员引导文档和 mnt 访问边界。
5. 若需要多后端/路径路由/上游 TLS/真正的 keep-alive 池，应在新的数据面设计阶段明确重写，不要基于当前只取 first backend 的结构做局部修补。
6. 统一前端与后端分页参数、错误码、代理头信任策略和 API 响应约定。
7. 不把旧项目文档中的数据库连接串、密码、SSH 密钥、生产 IP 等凭据复制到新工程；本文档已刻意不包含这些敏感信息。
