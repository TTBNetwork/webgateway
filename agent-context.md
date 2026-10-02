# agent-context.md

> 用途：保存当前工作上下文，便于下次会话快速接手。
> 生成时间：2026-10-02 05:52Z（UTC）。配套阅读：[agent.md](agent.md)、[STEP.md](STEP.md)、
> [CHANGELOG.md](CHANGELOG.md)、[ISSUES.md](ISSUES.md)、[PRODUCTION_READINESS.md](PRODUCTION_READINESS.md)。

## 1. 当前任务范围（来自 STEP.md）

| # | 事项 | 状态 |
|---|------|------|
| 修改-1 | 保证能在生产环境直接在线迁移并运行 | 代码与编排就绪；**生产迁移尚未执行** |
| 修改-2 | gateway 的 `operation was cancelled` | ✅ 已修复并端到端验证 |
| 修改-3 | 删除 `CONVERSATION.md`（融合进 ISSUES.md） | ✅ 已并入 ISSUES.md 附录 A 并删除 |
| 可选-1 | dashboard/frontend 的 yarn 换成 pnpm | ✅ 已完成（pnpm-lock.yaml 为唯一锁文件，CI 已切） |
| 目前-1 | 证书续签先不用管 | ✅ 按此执行 |
| 目前-2 | 压缩/清理数据库历史数据（面板可配，下限 3 个月） | ✅ 已实现（设置 → 数据保留，90~3650 天，默认 180；gateway 每小时清理） |
| 目前-3 | 面板可选配置 HTTP/HTTPS 代理以续签证书 | ✅ 核实无需改代码（reqwest 默认读 `HTTPS_PROXY` 等） |
| 目前(第九轮)-1 | 统计表 v2 + 按周轮换 | ⏳ **待确认**（大表分区化重写，需停机窗口；方案见 PRODUCTION_READINESS 第十节） |
| 目前(第九轮)-2 | 过期证书不加载 | ✅ 已完成（有有效证书则只用有效的；全过期才回退，附 3 个单测） |
| 目前(第九轮)-3 | 面板站点编辑/删除 + 每行三个布局 | ✅ 已完成（含 admin 才能删除、404 语义、搜索过滤） |
| 目前(第九轮)-4 | 迁移后查询优化（面板加速） | ⏳ 部分（低风险索引/查询可先做，其余等 v2） |
| 目前(第七轮)-1 | size 表要能"细化到每秒颗粒度" | ✅ 改为按 `(请求, 秒)` 聚合（第六轮曾过度聚合成一行、丢了秒级颗粒度，第七轮已纠正） |
| 目前(第七轮)-2 | 任一服务启动即自动迁移（无感） | ✅ `AutoMigrate` 成为默认；控制面表纳入共享迁移；compose 去掉一次性 migrate 服务 |
| 最后-1 | 把 STEP.md 内容融合进 agent.md | ✅ 已完成（只并入「需要修改/可选修改/最后」，不含凭据） |
| 最后-2 | 保存上下文到 agent-context.md | ✅ 本文件 |

## 2. 环境与凭据

| 项 | 值 |
|----|-----|
| 生产库（**可连，只读**） | `postgres://<user>:<pwd>@10.240.0.1:5432/postgres`（凭据见 STEP.md） |
| 生产库真实库名 | **`postgres`**（不是 `webgateway`；STEP.md 里写的库名不存在） |
| 生产主机 | `sj-pub`，Debian 13，**2 vCPU / 1.9 GiB 内存无 swap**，根分区 39G 用 50% |
| 生产库版本 | PostgreSQL 18.2 (Debian 18.2-1.pgdg12+1)，扩展 `btree_gin` / `uint128` / `plpgsql` |
| 生产库账号权限 | `root` 是 **superuser** —— 务必只读操作（STEP.md 要求"仅访问，不可修改"） |
| 开发/beta 库 | `postgres://webgateway:***@192.168.2.254:7777/webgateway_beta`（凭据见 `dev/environment`，已 gitignore） |
| 生产 SSH | `root@10.240.0.1`，密钥用 `~/.ssh/id_personal`（`id_ed25519` / `id_rsa` 均被拒） |
| SSH 可写路径 | 家目录只读，必须指定 `-o UserKnownHostsFile=$PWD/target/ssh/known_hosts`，否则连不上 |
| SSH 变更限制 | **执行任何变更命令前必须经用户确认**（STEP.md 要求） |
| 生产部署目录 | `/opt/webgateway/`（compose + `.env` + `data/`）|
| 生产镜像源 | `ghcr.milu.moe/ttbnetwork`（镜像站，**不是 ghcr.io**）；postgres 用 `ghcr.milu.moe/tianxiu2b2t` |
| 本机地址 | `192.168.2.9/24`，经 `192.168.2.1` 路由到 `10.240.0.1` |
| 测试域名 | `test-proxy.txit.top`、`test-vcmp.txit.top`、`tp.txit.top` |

> 端口变迁：STEP.md 最初写 `:5244`（从本机 Connection refused），后更新为 `:5432`。
> 注意 `10.240.0.1` 上**只有 `postgres` 这一个库**。

## 3. 生产库真实状态（2026-10-02 05:50Z 只读巡检）

### 数据量

| 表 | 行数 | 大小 |
|----|------|------|
| `access_request_logs` | ~2,300,593 | 1697 MB |
| `access_response_logs` | ~2,090,720 | 972 MB |
| `access_response_size_logs` | ~15,565,769 | **3606 MB** |
| `access_request_size_logs` | ~57,515 | 15 MB |
| 其余（certificates / users / websites / dns_providers / users_client_secrets / web_log） | 各 0~8 行 | < 100 kB |

### 🔴 缺失的列（新代码启动前必须迁移）

| 列 | 状态 | 不迁移的后果 |
|----|------|-------------|
| `users.role` | **缺失** | dashboard 登录时报 `column role does not exist` |
| `certificates.signing_started_at` | **缺失** | 证书签发认领写入失败 |

其余依赖列（`certificates.expires_at`、`access_request_logs.requested_at` / `remote_addr`）已存在。
视图 `users_info` / `qps_per_second` / `qps_per_5s` / `daily_traffic_by_website` 均存在，
`users_info` **不含 role 列**，迁移里用 `DROP VIEW` + `CREATE VIEW` 重建。

### 现状数据摘要

- 用户：仅 `admin` 一个（无 role 列）。
- 证书：2 张通配符证书（`*.ttb-network.top`、`*.atxa.top`、`*.txit.top`），
  `email` 与 `dns_provider_id` **均为 NULL**（因此不参与自动续签）；
  `6a2b8ef3…` 已于 2026-09-03 过期，`6a9b5b8e…` 到期 2026-11-26。
- 站点：8 个（hmos / vcmp / api / api-mcmod / s3 / storage / emcgold / *-emcgold）。
- 触发器：`{websites,certificates,dns_providers}` × `{_notify, _updated_at}`，共 6 个。

## 4. 本轮代码改动（详见 CHANGELOG.md 第四轮）

| 文件 | 改动 |
|------|------|
| `gateway/src/upstream.rs` | ① 连接任务结束后连接不得回池（否则后续请求投递到无接收者队列 → **永久挂起**）；② `try_send_request` 实现"复用连接已被上游关闭"时安全重试一次 |
| `gateway/src/upstream/connection.rs` | 新增 `task_exited` 标志与 `is_reused()`；`is_reusable()` = 「响应体读到 EOF **且** 驱动任务仍在运行」 |
| `crates/shared/src/database/access.rs` | 批量 INSERT 幂等化（`ON CONFLICT DO NOTHING`），消除"部分成功后整批重试永久失败" |
| `crates/shared/src/database/certificate.rs` | ① 认领加 10 分钟过期回收；② **修复 SELECT 漏选 `email` 导致自动续签 100% 失败** |
| `crates/shared/src/database.rs` | `verify_database_schema()` 增加**列**校验（原先只查表） |
| `docker-compose.yml` | 新增一次性 `migrate` 服务（`dashboard --migrate`），两服务等它成功后启动 |

### 关键验证结论

- 修复前：`curl` 60 次 = 30 成功 / 30 超时（**每隔一个请求超时**）。
  修复后：新连接模式 6/6、keep-alive 模式 6/6、`curl` 50/50 全部 200。
- 迁移在 beta 库上幂等通过（`gateway --migrate`、`dashboard --migrate` 均退出 0）。
- `cargo check --workspace --all-targets` 零错误；前端 `vue-tsc -b && vite build` 通过。

## 4.5 第六轮新增（保留期 + 存储膨胀修复）

- **`access_response_size_logs` 的 7 倍膨胀已修复**：原来 body 每读一个 chunk 就插一行
  （每个 chunk 一个新 ObjectId）。现在 gateway 用 `SizeAccumulator` 在内存里按 id 累加，
  每轮刷盘每 id 只落一行。生产实测该表 1600 万行/3.6GB（请求表 7.03 倍），且仍在以
  6.4 倍冗余增长，单条 `response_id` 最多挂 262791 行。**历史脏数据不会自动消失**，
  已存在的 1600 万行需要靠保留期（或手工清理）回收。
- **保留期**：`configurations` 表的 `access_log_retention`（`retention_days` 默认 180、
  夹紧 [90,3650]，`enabled` 默认 true）。gateway 每小时清理一次，用
  `locks::RETENTION` 的会话级 advisory lock 独占 —— **必须在同一条连接上取锁与解锁**，
  否则锁静默失效、清理停止。
- **`configurations` 表以前从未被创建**（initializer 没被调用过），现已在迁移中创建并加入
  `verify_database_schema` 的必需表。生产库目前仍**没有**这张表，迁移后才有。
- 前端新增「设置 → 数据保留」页（`pages/Dashboard/settings/DataRetention.vue`），
  API 在 `apis/settings.ts`。

## 4.6 第七轮新增（按秒颗粒度 + 无感迁移）

- **size 明细表按 `(请求, 秒)` 聚合**：`at_second`（截断到整秒）+ 唯一键
  `(xxx_id, at_second)` + `ON CONFLICT DO UPDATE body_length = body_length + EXCLUDED.body_length`。
  同一秒合并、跨秒分开 → 秒级颗粒度保留，同时消除按 chunk 的 7 倍膨胀。
  - 落库前必须用 `merge_same_second` 合并**批次内**同键行：PostgreSQL 单条
    `INSERT ... ON CONFLICT DO UPDATE` 不允许两次命中同一冲突键。
  - 累加写**必须整批单事务**，否则"部分成功后重试"会重复累加。
- **无感迁移**：`DbStartupMode` 默认由 `Serve` 改为 `AutoMigrate`；
  `DB_AUTO_MIGRATE=0` 是逃生开关。控制面表 DDL 在
  `crates/shared/src/database/dashboard_schema.rs`（原先在 dashboard 内，gateway 调不到）。
- **老库首次启动会慢**：迁移要补列 + 回填 + 合并同秒重复行 + 建唯一索引。
  生产库只读实测需合并 `access_response_size_logs` **约 417 万行**（298541 组）。
  合并取组内 `SUM`，统计不会变小；单事务，失败整体回滚。
- `docker-compose.yml` 已移除一次性 `migrate` 服务。

## 4.7 第九轮新增（证书过期过滤 + 站点编辑/删除）

- **证书过期过滤**：`gateway/src/sync/cert.rs` 的 `loadable_certificates(&[bool]) -> Vec<bool>`
  是唯一判定点：有未过期证书就只装载未过期的；全过期则全部装载（保可用性）。
  加/改 TLS 装载逻辑时先看这 3 个单测。
- **站点 CRUD**：`update_website` 是**整条覆盖**语义（不是合并），影响 0 行 → 路由回 **404**；
  删除需要 **admin**。路由：`GET /websites/{id}`、`POST /websites/{id}/update`、
  `POST /websites/{id}/delete`。
- **前端**：`AddWebsite.vue` 现在同时承担新增与编辑（传 `website` prop）；
  网站列表每行三个，搜索框已接上过滤。

## 5. 未解决 / 需注意

1. **release 构建受阻（与本次改动无关）**：`acmex 0.8.0` 硬编码 `aws-lc-rs` 的 `fips` feature，
   必然编译 `aws-lc-fips-sys`；本机 GCC 15.2 + binutils 2.46 拒绝其 `.data.rel.ro.local` 段
   （[aws-lc-rs#614](https://github.com/aws/aws-lc-rs/issues/614)）。CI 的 `ubuntu-latest` 大概能过，属撞运气。
2. **上游连接空闲复用率下降**：修复挂起的代价。连接任务结束即关闭连接；
   同一客户端 keep-alive 连接内的连续请求仍复用。恢复复用需把上游连接生命周期从客户端连接任务中拆出（重构）。
3. 访问日志表**无分区**（保留期清理已实现，分区方案仍未做，见 ISSUES.md 附录 A）。
   生产 `access_response_size_logs` 3606 MB；**当前最早数据 2026-04-25、跨度 160 天**，
   因此默认 180 天保留期暂时不会删任何数据 —— 要立刻回收空间需把保留期调到 90~160 天之间，
   或手工清理。
4. 生产两张证书的 `email` / `dns_provider_id` 都是 NULL，**自动续签不会生效**；
   `6a2b8ef3…` 已过期。上线后若期望自动续签，需要在面板补这两个字段。

## 6. 已确认的工程约束（踩过的坑）

- `tests/` 目录是运行期产物（证书/日志），**不是**测试目录，且被 gitignore；
  `cargo test` 的探针要放 `crates/shared/tests/`，用完必须删除（本轮已反复如此操作）。
- `gateway` crate **没有** `sqlx` 直接依赖，探针脚本不能用 `sqlx::`；需访问数据库时把探针放到 `shared`。
- `shared` 的 tokio 未开 `rt-multi-thread`/`macros`，`#[tokio::test]` 与 `Runtime::new()` 不可用，
  要用 `Builder::new_current_thread().enable_all().build()`。
- `TrySendError` 只实现 `Debug`（不实现 `Display`），且字段私有，必须用 `take_message()`。
- 后台长时间运行的进程（gateway / 上游）必须用受管后台任务启动，`nohup ... &` 会随 shell 退出被回收。
- 本机 curl 直连需加 `--noproxy '*'`，且请求要带 `-H 'Host: <站点域名>'` 才能命中站点路由。
- 改 beta 库做端到端验证后**务必复原**（本轮改了测试站点端口/上游，已还原）。
