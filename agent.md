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
| 部署 | Docker Compose（一次性 migrate + 4 容器）+ GitHub Actions 构建镜像到 GHCR |
| 当前版本 | `0.1.0`（见 [project.toml](project.toml)） |
| 当前阶段 | 开发中（未发布）。核心链路可用，存在停用模块与半成品 crate，详见 [TODO.md](TODO.md) |
| 生产可用性 | 2026-10-02 第四轮修复后，阻塞上线的代码缺陷已处理；**但 release 构建受阻于 `acmex → aws-lc-rs/fips`**（详见 CHANGELOG 与 ISSUES.md） |

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
cd dashboard/frontend && pnpm install --frozen-lockfile && pnpm build   # 包管理器为 pnpm（2026-10-02 起，原为 yarn）
```

### 前端包管理器

前端使用 **pnpm**（2026-10-02 从 yarn 迁移）。`pnpm-lock.yaml` 是唯一锁文件，`yarn.lock` /
`.yarnrc.yml` / `.yarn/` 已删除。

```bash
cd dashboard/frontend
pnpm install --frozen-lockfile   # CI 用这个；lockfile 与 package.json 不一致会直接失败
pnpm build                       # vue-tsc -b && vite build
pnpm typecheck                   # 仅类型检查
pnpm lint                        # eslint（注意：存量错误较多，见 ISSUES.md）
```

- pnpm 版本固定在 `package.json` 的 `packageManager` 字段（`pnpm@12.8.1`），CI 里由
  `pnpm/action-setup@v4` 使用同一版本。
- pnpm 12 起 `overrides` 等设置写在 `pnpm-workspace.yaml`，**不再**读 `package.json` 的 `pnpm` 字段。
- `storeDir` 指向 `../../target/pnpm-store`（本开发环境家目录只读）。

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
docker compose up -d      # 一次性 migrate → postgres + gateway + dashboard-backend + dashboard-frontend
```

- **无需单独跑迁移**：两个服务默认都是 `AutoMigrate` —— 启动时在
  `locks::SCHEMA_INIT` 事务级 advisory lock 下补齐**完整** schema，然后进入服务。
  因此 gateway 与 dashboard **谁先启动都一样**，全新库直接 `docker compose up -d` 即可。
  迁移幂等，重复 `up` 安全。
- 只想服务、不执行 DDL：设 `DB_AUTO_MIGRATE=0`（逃生开关；此时必须自行保证 schema 已就绪）。
- 独立迁移 Job（例如 K8s）：`<镜像> --migrate`，只迁移后退出。
- 首次在**老库**上启动会稍慢：size 明细表要补 `at_second` 列、合并历史重复行并建唯一索引
  （生产库实测约 417 万行需要合并），见下方「size 明细表」一节。
- 端口：网关 `80/443`，前端 `7173→4173`（nginx），后端容器内 `3000`。
- 镜像推送由 [.github/workflows/build.yml](.github/workflows/build.yml) 在 push `master` 或 `v*` tag 时触发，推到 GHCR。

### 生产部署现状（2026-10-02 只读排查，`10.240.0.1`）

> 这一节由 [STEP.md](STEP.md) 的「授予权限」整理而来，**不含任何凭据**。

| 项 | 现状 |
|----|------|
| 主机 | `sj-pub`，Debian 13 (trixie)，内核 6.12，**2 vCPU / 1.9 GiB 内存**（无 swap），根分区 39G 已用 50% |
| 部署目录 | `/opt/webgateway/`（`docker-compose.yml` + `.env` + `data/`） |
| 运行容器 | `webgateway-pg` / `-gateway` / `-dashboard-backend` / `-dashboard-frontend`，均为 `Up 4 days` |
| 镜像来源 | **`ghcr.milu.moe`**（GHCR 镜像站），不是 `ghcr.io`：`IMAGE_PREFIX=ghcr.milu.moe/ttbnetwork`、`POSTGRES_IMAGE_PREFIX=ghcr.milu.moe/tianxiu2b2t` |
| 端口 | 80 / 443（网关）、7173（前端）、5432（Postgres 直接暴露到公网） |
| **当前版本** | 镜像 3 周前构建，**早于审计修复**：日志里没有新代码的 `Database startup mode: Serve` 特征 |
| 迁移入口 | **未部署**：`/opt/webgateway/.env` 没有 `DB_AUTO_MIGRATE`，compose 里也没有 migrate 服务 |
| 库名注意 | 生产库名是 **`postgres`**，不是 `webgateway`（连接串写 `.../webgateway` 会报 database does not exist） |

**上线时要做的事**（新镜像默认自迁移，不再需要单独跑迁移服务）：

1. 部署新镜像即可 —— 两个服务默认 `AutoMigrate`，会在 advisory lock 下把生产库缺失的对象
   （`users.role`、`certificates.signing_started_at`、`configurations` 表、
   size 表的 `at_second` + 唯一索引）全部补齐。
2. **首次启动会明显变慢**：size 明细表要合并历史重复行并建唯一索引，生产库实测
   `access_response_size_logs` 有 298541 组重复、约 **417 万行**需要合并（16M 行表上的一次性代价，
   会消耗较多 WAL 与磁盘 IO）。建议选低峰期、并确认磁盘余量（当时约剩 19 GB）。
   迁移是单事务：中途失败会整体回滚，重跑安全。
3. `/opt/webgateway/.env` 里**不要**设置 `DB_AUTO_MIGRATE=0`（那是"只服务不迁移"的逃生开关）。

### 环境变量

| 变量 | 作用 |
|------|------|
| `DATABASE_URL` | PostgreSQL 连接串（后端与网关） |
| `DATABASE_MAX_CONNECTIONS` | 连接池上限（默认 10） |
| `DASHBOARD_API_PORT` | 控制面 API 端口（默认 3000） |
| `TOKEN_EXPIRES` | JWT 有效期秒数（默认 7 天） |
| `BACKEND_URL` | 前端 nginx / Vite 代理的后端地址 |
| `SUBNET_PREFIX`、`POSTGRES_*`、`IMAGE_PREFIX` | 仅 `docker-compose.yml` 使用 |
| `HTTP_PROXY` / `HTTPS_PROXY` / `NO_PROXY` | **可选**：ACME 证书签发与 DNS API 调用走代理（reqwest 默认读取，见下） |
| `DB_AUTO_MIGRATE` | 默认（不设置）即「先迁移再服务」；设为 `0`/`false` 则只服务、不执行 DDL |

## 相关文档

- 问题清单（审计报告）：[ISSUES.md](ISSUES.md)
- 存储优化 / 日志表 v1→v2 迁移方案（外部 AI 产出，**待评审**）：见 [ISSUES.md](ISSUES.md) **附录 A**（原 `CONVERSATION.md`，已合并）
- 待办清单：[TODO.md](TODO.md)
- 变更日志：[CHANGELOG.md](CHANGELOG.md)
- 生产上线评估（第四轮）：[PRODUCTION_READINESS.md](PRODUCTION_READINESS.md)
- 工作上下文（给下次会话）：[agent-context.md](agent-context.md)
- 当前任务单：[STEP.md](STEP.md)（其「需要修改 / 可选修改 / 最后」三节已并入本文件）

## 任务单（原 STEP.md）

> 由 `STEP.md` 整理而来。**「授予权限」一节只保留非凭据事实（见上方「生产部署现状」），
> 连接串与口令不入库**。STEP.md 在 2026-10-02 更新过一次：原来的
> 「需要修改 / 可选修改」换成了「目前」，下方分区照此调整。

### 目前（当前任务，STEP.md 2026-10-02 最新版）

| # | 任务 | 状态 |
|---|------|------|
| 1 | 统计表改 v2 并支持**按周轮换** | ⏳ **待确认**：1380 万行 / 6.3 GB 的分区化重写（移除外键、主键加分区键、数据搬迁、读写切换），方案与风险见 [PRODUCTION_READINESS.md](PRODUCTION_READINESS.md) 第十节、DDL 见 [ISSUES.md](ISSUES.md) 附录 A |
| 2 | **过期的证书不加载**（除非一张有效的都没有） | ✅ 已完成（第九轮）：`sync_certificates` 先解析并标注过期，有有效证书就只装载有效的；全过期才全部回退装载并打 ERROR。规则抽成 `loadable_certificates()` + 3 个单测 |
| 3 | 面板**支持编辑/删除站点** + 网站排布（每行三个，不足的占满） | ✅ 已完成（第九轮）：新增 `GET /websites/{id}`、`POST /{id}/update`（需 user）、`POST /{id}/delete`（需 admin，删除前确认）；前端复用 `AddWebsite.vue` 做编辑对话框；列表改为每行三个（窄屏两列/单列）并接上搜索过滤 |
| 4 | 统计表迁移后做查询优化（加速面板） | ⏳ 部分：低风险的查询/索引优化可先做；依赖 v2 的部分等迁移 |

### 历史任务（STEP.md 更早版本）

| 5 | （更早版本）证书续签问题先不用管 | ✅ 按此执行 |
| 6 | （更早版本）压缩数据库数据 / 清理历史日志（面板可配，下限 3 个月） | ✅ 已完成（第六轮）：控制面板「设置 → 数据保留」90~3650 天（默认 180），gateway 每小时按批清理 |
| 7 | （更早版本）面板可选配置 HTTP/HTTPS 代理以续签证书 | ✅ 核实后确认**无需改代码**：acmex 未调 `.no_proxy()`，reqwest 默认读 `HTTPS_PROXY` 等环境变量 |

### 历史任务（STEP.md 旧版：需要修改 / 可选修改）

| # | 任务 | 状态 |
|---|------|------|
| 1 | 保证能在生产环境直接在线迁移并运行 | ✅ 代码与部署编排就绪（migrate 服务 + 列校验）；**生产迁移尚未执行** |
| 2 | 修 gateway 的 `operation was cancelled` | ✅ 已修复；生产日志实证该错误出现 **8099 次** |
| 3 | 删除 `CONVERSATION.md`（融合进 `ISSUES.md`） | ✅ 已并入 [ISSUES.md](ISSUES.md) **附录 A** 并删除原文件 |
| 4 | （旧版「可选修改」）`dashboard/frontend` 的 yarn 换成 pnpm | ✅ 已完成：`pnpm-lock.yaml` 为唯一锁文件，CI 改用 `pnpm/action-setup@v4` + `--frozen-lockfile` |

### 最后（STEP.md 两次版本都有）

| # | 任务 | 状态 |
|---|------|------|
| 1 | 把 `STEP.md` 内容融合进本文件 | ✅ 本节；「授予权限」仅保留非凭据事实 |
| 2 | 把已读取的内容保存到 `agent-context.md` | ✅ 见 [agent-context.md](agent-context.md) |

### 生产环境排查要点（本次只读排查的发现）

- **`operation was canceled` 在生产是真实且高频的**：网关日志 46 万行中出现
  `Failed to send request: operation was canceled` **8099 次**，另有 7897 次
  `hyper::Error(Canceled, IncompleteMessage)`。这正是第四轮修复的目标。
- **`qps_per_5s` 视图仍在被旧镜像使用**：dashboard 日志显示该查询单次耗时 **4.66 秒**
  （阈值 1s），且前端每 5 秒轮询一次 —— 与 ISSUES.md P0-3 的判断一致。新代码已改为直接过滤
  `requested_at`。
- **TLS 握手失败占日志主体**：`BadCertificate` 25.7 万次、`tls handshake eof` 10.4 万次、
  `NoCipherSuitesInCommon` 6.6 万次。多数是扫描器/爬虫探测，但数量级值得单独排查。
- 生产镜像 3 周前构建、早于全部审计修复；`/opt/webgateway/.env` 无 `DB_AUTO_MIGRATE`，
  compose 无 migrate 服务。

## 访问日志保留期（第六轮）

| 项 | 值 |
|----|-----|
| 配置位置 | `configurations` 表，key = `access_log_retention` |
| 字段 | `retention_days`（默认 `180`，夹紧到 `[90, 3650]`）、`enabled`（默认 `true`） |
| 执行者 | **gateway**（数据面）每小时一次；用 `locks::RETENTION` 的 `pg_try_advisory_lock` 保证单实例执行 |
| 删除顺序 | size 明细（`access_response_size_logs` → `access_request_size_logs`）→ 响应 → 请求；**顺序不能改**，否则撞外键 |
| 批量 | 每轮 5 万行、每次最多 20 轮；面板"立即清理"每次最多 25 万行 |
| API | `GET/POST /settings/retention`、`POST /settings/retention/prune`（写操作需 `user` 及以上） |
| 前端 | 设置 → 数据保留 |

**注意**：会话级 advisory lock 必须**在同一条连接**上获取与释放。早期实现用连接池
`fetch_one` 取锁、再 `execute` 解锁，两次很可能落在不同连接上 —— 解锁语句释放的是那条连接
自己的锁（它并未持有），而真正持有的锁要等连接被复用/关闭才释放，于是后续每一轮都拿不到锁、
清理**静默停止**。`prune_access_logs` 现在显式 `acquire` 一条连接贯穿始终。

## ⚠️ 启动路径上的迁移必须廉价（2026-10-02 生产事故的教训）

**第七轮曾把「全表回填 + 合并 417 万重复行 + 建 1600 万行唯一索引」放进启动路径的单个事务，
结果生产两个服务全部起不来**：迁移持着 `SCHEMA_INIT` 排他锁跑数分钟 → 另一个进程无限期等锁 →
容器被健康检查判定为"起不来"并重启 → 重启让迁移整体回滚、**重来一遍**，死循环。
恢复动作：让迁移尽快完成（或改用廉价迁移），**不要反复重启**。

**硬性规则**：启动路径（`inner_init_database_with`）里只允许做**与表大小无关**的操作 ——
建表、加**可空**列、建**部分**索引、`CREATE INDEX IF NOT EXISTS` 于小表。
任何需要**回填、去重、在大表上建全量索引**的动作，都必须放到服务起来之后的**后台任务**里分批执行。

配套的两个防呆：
- 迁移取锁有 60 秒上限（`SET LOCAL lock_timeout`），超时快速失败并给出提示，
  不会再静默挂死；
- size 表的秒级归一改用**部分唯一索引**（只覆盖新行），因此不需要回填历史数据。

## size 明细表：按秒颗粒度（第七轮）

`access_request_size_logs` / `access_response_size_logs` 的用途是**按秒统计请求/响应体大小**。

| 项 | 说明 |
|----|------|
| 唯一键 | `(request_id, at_second)` / `(response_id, at_second)`，`at_second` 截断到整秒 |
| 写入 | gateway 侧 `SizeAccumulator` 按 `(id, 秒)` 在内存累加 → `ON CONFLICT ... DO UPDATE SET body_length = body_length + EXCLUDED.body_length` |
| 粒度 | 同一秒内合并成一行；**跨秒仍分开**，秒级颗粒度不丢 |
| 分批限制 | PostgreSQL **不允许**一条 INSERT 里两次命中同一冲突键（`cannot affect row a second time`），因此落库前用 `merge_same_second` 在内存合并同键行 |
| 事务 | 累加写入**不做分块提交**：整批单事务，否则"部分成功后重试"会重复累加 |
| 父行守卫 | `INSERT ... SELECT ... WHERE EXISTS (主表)`：size 表对主表有外键，父行缺失时必须**跳过**而不是抛错 —— 抛错会让整批永久卡在重试队列里（生产实测过） |
| 索引兼容 | 写入用 `DELETE 该行 + INSERT 累加值`，**不用** `ON CONFLICT`：全量唯一索引与部分唯一索引对 `ON CONFLICT` 的语法要求互不兼容，而生产库里两种都可能存在 |
| 历史行 | `at_second` 为 **NULL**（不加 `NOT NULL`、不回填、不去重）—— 见下方"部分唯一索引" |
| 部分索引 | 唯一索引带 `WHERE at_second IS NOT NULL`：只约束**新行**，因此迁移不需要扫全表 |
| ON CONFLICT | 必须带**同样的谓词** `WHERE at_second IS NOT NULL`，否则推断不出索引（实测报错） |

**为什么必须带"秒"**：早期版本按 chunk 逐行插入（每 chunk 一个新 ObjectId），生产库因此有
1600 万行 / 3.6 GB（请求主表的 7 倍，单条 `response_id` 最多 262791 行）。只按 id 聚合成一行
虽然最省空间，但会**彻底丢掉秒级颗粒度**；按 `(id, 秒)` 聚合两者兼顾。

**为什么用部分索引而不是全量唯一索引 + 回填**：全量索引要求历史行也满足唯一性，
于是必须回填 `at_second` 并合并 417 万重复行 —— 这正是第八轮事故的原因。
部分索引把这些代价降到零：历史行 `at_second` 为 NULL、不参与索引，新行才有值。
代价是历史行没有按秒归一（它们的秒级信息本来就在 `created_at` 上，逐个 chunk 一行）。

## 可选：让 ACME 走代理（第六轮核实）

acmex 用 `reqwest::Client::builder()` 构造 HTTP 客户端且**没有**调用 `.no_proxy()`，
因此 reqwest 会默认读取 `HTTP_PROXY` / `HTTPS_PROXY` / `ALL_PROXY` / `NO_PROXY`。
给 dashboard-backend 容器设置这些变量即可让证书签发与 DNS API 调用走代理，**无需改代码**。

## 已知风险提示（给后续 AI）

以下问题已在 [ISSUES.md](ISSUES.md) 中详述，改动相关代码时务必注意。
**状态**：2026-10-02 第四轮修复后，第 1/2/4/5 条已处理，状态以 [ISSUES.md](ISSUES.md)「修复状态」表为准。

1. ~~**两个进程都会执行 DDL**~~ **已修复（P0-7）**：DDL 收敛到**单一迁移入口**，全程持有 `locks::SCHEMA_INIT` 事务级 advisory lock。**第七轮起默认就是自动迁移**（配置见上表），两个服务谁先启动都会得到同一份完整 schema；`DB_AUTO_MIGRATE=0` 可退回"只校验不建表"。修改任何初始化 SQL 时请同步修改 `inner_init_database_with` 里的所有 initializer（它们都接收 `&mut Transaction`）；控制面表的 DDL 在 `crates/shared/src/database/dashboard_schema.rs`。
2. ~~**访问日志刷盘是"先删内存后写库"**~~ **已修复（P0-1）**：现在是「先写库成功、再清内存」，失败批次留在 `inflight` 下一轮重试。改动刷盘逻辑时**不要**破坏这个顺序；注意 `PendingBuffer` 的 `inflight` 同时承担重试队列的角色。**并且**：批量 INSERT 必须保持幂等（`ON CONFLICT DO NOTHING`），否则"部分成功后整批重试"会永久毒化该队列（详见 CHANGELOG 第四轮）。
3. **`qps_per_second` / `qps_per_5s` 视图会全表扫描**（仍在仓库中，但**已不再被使用**）：QPS 查询改为直接过滤 `requested_at`。若要重新启用视图，先确认谓词 sargable，且注意前端每 5 秒轮询一次。
4. ~~**`gateway/src/proxy/` 是已停用的死代码**~~ 仍是死代码（未清理），当前生效的是 `gateway/src/upstream/`。修改代理逻辑时不要改错目录。
5. ~~**连接池健康检查是无效的**~~ **已修复（P0-5）**：`write(&[])` 恒真检查已删除，改成零长度 `read` 探活；连接只有在响应体读到 EOF 后才允许归还。**新的注意点（第四轮）**：`is_reusable()` 现在是「响应体读到 EOF **且** 连接任务仍在运行」两个条件。**连接任务一旦结束就必须关闭连接** —— 把已无 future 驱动的连接放回池中，会让下一个请求投递到没有接收者的队列并**永久挂起**（实测：每隔一个请求超时）。`task_exited` 标志就是为此存在的，不要绕过它。
6. **新增（P1-16）权限模型**：`users.role` ∈ {`admin`,`user`,`view`}。受保护的 handler 必须从 `Authorizer` 提取器取身份并显式调用 `require_write()` / `require_admin()`，**不要**只用 `AuthJWTInfoExtract` 就认为已经鉴权。新增路由时请一并决定所需角色。
7. **新增（P0-6 残留）**：连接池上限已生效（默认 256），但**空闲复用率进一步下降**（第四轮修复挂起的代价）：连接任务结束即关闭连接，所以"客户端请求结束后的空闲复用"不再发生（同一客户端 keep-alive 连接内的连续请求仍复用）。并发超过上限时请求会排队并在 5s 后快速失败（502），这是有意的背压。要恢复复用需让上游连接生命周期脱离客户端连接任务，属重构。
8. **新增**：站点匹配前会去掉 Host 头端口（`strip_port()`），因此站点配置里**不要**写带端口的 host。
9. **新增（第四轮）**：证书签发认领有 `STALE_SIGNING_CLAIM = 10 minutes` 的过期回收（`crates/shared/src/database/certificate.rs`）。改这个常量前先确认正常签发耗时 —— 调得太短会导致多实例重复签发、白白消耗 ACME 配额。

## 启动模式（P0-7）

| 进程 / 场景 | 启动方式 | 行为 |
|------------|---------|------|
| **任一服务（默认）** | 直接启动 | `AutoMigrate`：在 advisory lock 下执行**全部** DDL（含控制面表），再进入服务 |
| 只服务不迁移 | `DB_AUTO_MIGRATE=0` | `Serve`：只校验表**与关键列**，缺失时明确报错（逃生开关） |
| 独立迁移 Job | `--migrate` | 只执行迁移然后退出 |
| Docker Compose | `docker compose up -d` | 两个服务各自 `DB_AUTO_MIGRATE=1`；无一次性 migrate 服务 |

迁移入口 `inner_init_database_with` 现在包含：`configurations` → `dns_providers` →
`certificates` → `websites` → `access_*`（含 size 表按秒迁移）→ **`users` /
`users_client_secrets` / `web_log`**（控制面表，见 `crates/shared/src/database/dashboard_schema.rs`）。
控制面表的 DDL 之所以从 dashboard 挪到 `shared`，就是为了让两个进程执行**同一份**迁移、
「谁先启动都行」—— 原先 gateway 先启动会缺 `users` 表。

`gateway` 与 `dashboard` 的迁移共用 `shared::database::locks::SCHEMA_INIT` 这一把锁，因此可以安全地并发启动。
