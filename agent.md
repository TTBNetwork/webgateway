# WebGateway

## 项目概述

WebGateway 是一个自建的**反向代理 / 网关系统**，由「数据面网关 + 控制面面板」两部分组成：

- **数据面（gateway）**：基于 hyper + tokio-rustls 的 HTTP(S) 反向代理，按 TLS SNI / Host 路由到上游，支持 TLS 证书热加载、上游连接池、访问日志统计。
- **控制面（dashboard backend + frontend）**：axum 提供 REST API，Vue 3 面板管理站点、证书、DNS 服务商、访问日志统计与用户认证（JWT + TOTP）。

| 维度 | 说明 |
|------|------|
| 主语言 | Rust（edition 2024，Cargo workspace，resolver 3） |
| 前端 | Vue 3 + TypeScript + Vite（`vue-router` / `echarts` / `ky` / `vue-i18n`） |
| 数据库 | PostgreSQL（`sqlx` 0.8，依赖 `uint128`、`btree_gin` 扩展，使用 LISTEN/NOTIFY 触发器做配置同步） |
| 运行时 | Tokio 异步运行时；Unix Socket 用于本地运维通道（mnt） |
| 部署 | Docker Compose（4 容器）+ GitHub Actions 构建镜像到 GHCR |
| 当前版本 | `0.1.0`（见 [project.toml](project.toml)） |
| 当前阶段 | 开发中（未发布）。核心链路可用，存在停用模块与半成品 crate，详见 [TODO.md](TODO.md) |

## 目录结构

```text
webgateway/
├── .github/
│   ├── actions/                  # 复合 Action：build / build-rust / build-frontend / setup-* / download-maxmind
│   └── workflows/build.yml       # push master 或打 v* tag 时构建并推送镜像
├── assets/
│   ├── error_pages/              # 网关错误页前端（Vite + TS）
│   ├── ipdb/                     # IP 库运行时资源目录（构建时注入）
│   └── sqls/access_init.sql      # 访问日志建表 SQL
├── crates/
│   ├── acme/                     # ACME 证书申请（半成品）
│   ├── dnsprovider/              # DNS 服务商 API 封装（DNSPod/Tencent）
│   ├── protocols/                # PROXY protocol 与 TLS ClientHello/SNI 解析
│   ├── shared/                   # 数据库层、数据模型、日志、监听器、站点工具
│   ├── simple_shared/            # 低依赖共享类型（ObjectId、mnt 协议、版本信息）
│   └── utils/                    # 通用工具函数（hex、脱敏、backlog）
├── dashboard/
│   ├── backend/                  # 控制面 API 服务（axum，默认端口 3000）
│   └── frontend/                 # 控制面前端（Vue 3 + Vite）
├── dev/                          # 本地开发启动脚本（gateway / dashboard_backend / dashboard_frontend / mnt）
├── gateway/                      # 数据面网关入口（80/443）
├── ipdb/                         # GeoIP / IP 库下载与生成工具（Python + uv）
├── mnt/                          # 运维 CLI 客户端（通过 Unix Socket 通信）
├── tests/                        # 运行期产物（证书、启动日志），非测试用例，已被 gitignore
├── Cargo.toml                    # Rust workspace 定义
├── project.toml                  # 项目名与版本（build.rs 注入 WG_VERSION / WG_PROJECT_NAME）
├── docker-compose.yml            # 生产部署编排
└── .env.default                  # 部署环境变量模板
```

## 子项目 / 模块作用

| 路径 | 作用 |
|------|------|
| [gateway/](gateway) | 数据面网关：监听 80/443，按 SNI/Host 路由并转发到上游，记录访问日志 |
| [gateway/src/upstream.rs](gateway/src/upstream.rs) | 当前生效的请求处理主链路：HTTP/HTTPS 转发、WebSocket 升级、连接复用 |
| [gateway/src/upstream/connection.rs](gateway/src/upstream/connection.rs) | 上游连接池（`UpstreamConnectionPool`） |
| [gateway/src/upstream/protocols.rs](gateway/src/upstream/protocols.rs) | 前置协议探测：TLS SNI 提取、PROXY protocol（尚未实现） |
| [gateway/src/sync/](gateway/src/sync) | 从数据库同步站点与证书；证书热加载通过 `ResolvesServerCert` 实现 |
| [gateway/src/access.rs](gateway/src/access.rs) | 访问日志采集与批量落库（请求/响应大小统计） |
| [gateway/src/transport.rs](gateway/src/transport.rs) | 带统计功能的响应 Body 包装类型 |
| [gateway/src/proxy/](gateway/src/proxy) | **已停用**的旧代理实现，与 `upstream` 功能重叠（`main.rs` 中 `pub mod proxy;` 被注释） |
| [crates/shared/](crates/shared) | 共享库：数据库连接与初始化、`models` 数据模型、日志、双栈监听器、favicon 抓取 |
| [crates/simple_shared/](crates/simple_shared) | 不依赖 sqlx/tokio 重生态的共享部分：`ObjectId`、mnt 通信协议、版本信息 |
| [crates/protocols/](crates/protocols) | TLS ClientHello / SNI 解析；PROXY protocol v1、v2 |
| [crates/dnsprovider/](crates/dnsprovider) | DNS 服务商抽象与 DNSPod（腾讯云）实现，用于 ACME DNS-01 挑战 |
| [crates/acme/](crates/acme) | ACME 证书构造（`CertificateBuilder`、域名哈希），**半成品** |
| [crates/utils/](crates/utils) | 通用工具：bytes/hex 转换、敏感信息脱敏、监听 backlog 常量 |
| [dashboard/backend/](dashboard/backend) | 控制面 API：认证（JWT + TOTP）、角色授权（`admin`/`user`/`view`）、站点/证书/DNS 提供商/日志路由、GeoIP 查询 |
| [dashboard/backend/src/router/](dashboard/backend/src/router) | REST 路由：`/websites`、`/logs`、`/dnsproviders`、`/certificates`、`/access`，认证挂在 `/auth` |
| [dashboard/backend/src/ip/](dashboard/backend/src/ip) | IP 归属查询：MaxMind MMDB + CZ88（纯真库），`ip2region` 已停用 |
| [dashboard/frontend/](dashboard/frontend) | 面板前端：登录、站点管理、证书管理、DNS 提供商、访问统计（QPS/地图） |
| [mnt/](mnt) | 运维 CLI，通过 `/tmp/webgateway-mnt.sock` 与后端通信（如获取管理员 TOTP） |
| [ipdb/](ipdb) | Python 工具：从 GitHub 拉取 MaxMind / ip2region / CZ88 数据库并输出到 `dashboard/backend/assets/ipdb/` |
| [assets/error_pages/](assets/error_pages) | 网关错误页静态站点 |
| [dev/](dev) | 本地开发脚本（`source dev/environment` 后运行对应二进制） |

## 关键入口与常用命令

### 关键入口文件

- 网关：`gateway/src/main.rs`（`cargo run -p gateway`）
- 控制面 API：`dashboard/backend/src/main.rs`（`cargo run -p dashboard`）
- 控制面前端：`dashboard/frontend/src/main.ts`
- 运维 CLI：`mnt/src/main.rs`（`cargo run -p webgateway-mnt`）
- 配置加载：`gateway/src/config.rs`、`dashboard/backend/src/config.rs`（优先读工作目录下的 `config.toml`，缺失则回退环境变量 / 默认值）

### 启动（本地开发）

```bash
# 需要先准备数据库连接：dev/environment 中导出 DATABASE_URL
./dev/gateway              # 数据面网关
./dev/dashboard_backend    # 控制面 API（默认 3000）
./dev/dashboard_frontend   # 前端 dev server（Vite，经 /api 代理到 BACKEND_URL）
./dev/mnt                  # 运维 CLI
```

### 构建

```bash
# 后端 / 网关 / CLI
cargo build --release --bin dashboard --bin gateway --bin webgateway-mnt

# 前端
cd dashboard/frontend && yarn install && yarn build
```

### 测试

- **当前没有自动化测试**（原状态；2026-10-02 第三轮新增了 2 个测试：`gateway` 的 `strip_port` 单元测试、`dashboard` 的授权链路集成测试）；根目录 `tests/` 存放的是证书与启动日志等运行期产物，非测试用例。
- 替代校验手段：

```bash
cargo check --workspace
cargo clippy --workspace --all-targets
cd dashboard/frontend && npx vue-tsc -b && npx eslint .
```

- 授权集成测试需要真实数据库（未设置 `DATABASE_URL` 会自动跳过）：

```bash
source dev/environment && cargo test -p dashboard --bin dashboard -- --nocapture
```

### 部署

```bash
cp .env.default .env      # 至少设置 SUBNET_PREFIX、POSTGRES_PASSWORD
docker compose up -d      # postgres + gateway + dashboard-backend + dashboard-frontend
```

- 端口：网关 `80/443`，前端 `7173→4173`（nginx），后端容器内 `3000`。
- 镜像推送由 [.github/workflows/build.yml](.github/workflows/build.yml) 在 push `master` 或 `v*` tag 时触发，推到 GHCR。

### 环境变量

| 变量 | 作用 |
|------|------|
| `DATABASE_URL` | PostgreSQL 连接串（后端与网关） |
| `DATABASE_MAX_CONNECTIONS` | 连接池上限（默认 10） |
| `DASHBOARD_API_PORT` | 控制面 API 端口（默认 3000） |
| `TOKEN_EXPIRES` | JWT 有效期秒数（默认 7 天） |
| `BACKEND_URL` | 前端 nginx / Vite 代理的后端地址 |
| `SUBNET_PREFIX`、`POSTGRES_*`、`IMAGE_PREFIX` | 仅 `docker-compose.yml` 使用 |

## 相关文档

- 问题清单（审计报告）：[ISSUES.md](ISSUES.md)
- 存储优化 / 日志表 v1→v2 迁移方案（外部 AI 产出，**待评审**）：[CONVERSATION.md](CONVERSATION.md)
- 待办清单：[TODO.md](TODO.md)
- 变更日志：[CHANGELOG.md](CHANGELOG.md)

## 已知风险提示（给后续 AI）

以下问题已在 [ISSUES.md](ISSUES.md) 中详述，改动相关代码时务必注意。
**状态**：2026-10-02 第三轮修复后，第 1/2/4/5 条已处理，状态以 [ISSUES.md](ISSUES.md)「修复状态」表为准。

1. ~~**两个进程都会执行 DDL**~~ **已修复（P0-7）**：DDL 收敛到迁移入口（`--migrate` / `DB_AUTO_MIGRATE=1`），全程持有 `locks::SCHEMA_INIT` 事务级 advisory lock；服务进程默认 `Serve` 模式，只校验不建表。修改任何初始化 SQL 时请同步修改所有 initializer（它们现在都接收 `&mut Transaction`）。
2. ~~**访问日志刷盘是"先删内存后写库"**~~ **已修复（P0-1）**：现在是「先写库成功、再清内存」，失败批次留在 `inflight` 下一轮重试。改动刷盘逻辑时**不要**破坏这个顺序；注意 `PendingBuffer` 的 `inflight` 同时承担重试队列的角色。
3. **`qps_per_second` / `qps_per_5s` 视图会全表扫描**（仍在仓库中，但**已不再被使用**）：QPS 查询改为直接过滤 `requested_at`。若要重新启用视图，先确认谓词 sargable，且注意前端每 5 秒轮询一次。
4. ~~**`gateway/src/proxy/` 是已停用的死代码**~~ 仍是死代码（未清理），当前生效的是 `gateway/src/upstream/`。修改代理逻辑时不要改错目录。
5. ~~**连接池健康检查是无效的**~~ **已修复（P0-5）**：`write(&[])` 恒真检查已删除，改成零长度 `read` 探活；连接只有在响应体读到 EOF 后才允许归还。**新的注意点**：`PooledUpstreamConnection` 的 `reusable` 标志是防串包的唯一防线，任何"提前归还"的改动都可能重新引入跨租户数据泄露。
6. **新增（P1-16）权限模型**：`users.role` ∈ {`admin`,`user`,`view`}。受保护的 handler 必须从 `Authorizer` 提取器取身份并显式调用 `require_write()` / `require_admin()`，**不要**只用 `AuthJWTInfoExtract` 就认为已经鉴权。新增路由时请一并决定所需角色。
7. **新增（P0-6 残留）**：连接池上限已生效（默认 256），但空闲复用率仍低 —— 连接由 hyper 的 `Connection` future 持有最长 120s。并发超过上限时请求会排队并在 5s 后快速失败（502），这是有意的背压而非 bug。
8. **新增**：站点匹配前会去掉 Host 头端口（`strip_port()`），因此站点配置里**不要**写带端口的 host。

## 启动模式（P0-7）

| 进程 / 场景 | 启动方式 | 行为 |
|------------|---------|------|
| 数据面 gateway | 默认 | `Serve`：只校验 schema，缺失表时明确报错 |
| 控制面 dashboard | 默认 | `Serve`：同上（但会引导默认管理员账号） |
| 任一进程（单实例 / 开发） | `DB_AUTO_MIGRATE=1` | 在 advisory lock 下执行全部 DDL，再进入服务 |
| 独立迁移 Job | `--migrate` | 只执行迁移然后退出 |

`gateway` 与 `dashboard` 的迁移共用 `shared::database::locks::SCHEMA_INIT` 这一把锁，因此可以安全地并发启动。
