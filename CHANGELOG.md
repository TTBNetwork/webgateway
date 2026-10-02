# Changelog

本项目所有值得记录的变更都写入本文件。版本号取自 [project.toml](project.toml)（当前 `0.1.0`）。
条目前缀统一使用：**AI 修改** / **新增** / **变更** / **修复** / **移除**，标注「AI 修改」的条目由 AI 助手生成或变更。

## [未发布]

### 2026-10-02（第三轮：P0 止血 + 安全 + 前端放大器修复）

本轮按 [ISSUES.md](ISSUES.md)「建议的修复顺序」实施，范围 = **P0 止血 + 安全问题 + 前端放大器 + 角色权限模型**；
**不含** [CONVERSATION.md](CONVERSATION.md) 的分区/冷迁移方案（需停机迁移，另行排期）。

- 修复（P0-1，**数据丢失**）：访问日志刷盘改为「**先写库成功、再从内存移除**」。原实现先 `retain` 删内存再 `insert`，写库失败即永久丢日志且不重试。现在每类日志有 `pending`/`inflight` 两个槽位：写成功才 `commit`，失败则批次留在内存下一轮重试，并记录连续失败轮数与内存积压规模。`gateway/src/access.rs` 基本重写。
- 修复（P0-1 补充）：`body_length` 的批量 UPDATE 原先在「请求行尚未落库」时会命中 0 行、静默丢失大小统计；由于 `access_request_logs.id` 是主键，超集行仅补 `body_length`、其余为 NULL，实际不会插出孤儿行。已通过真实数据库验证（`access_request_size_logs` 与主表 `body_length` 均正确落库）。
- 修复（P0-2）：`insert_batch_access_requests` / `insert_batch_access_responses` / 两个 size 增量 INSERT 按 ≤1000 行分块；两个 `body_length` 批量 UPDATE 由 `CASE WHEN` 改为 `FROM (VALUES ...)`，绑定数从 3/行降到 2/行且不再重复绑定 id。彻底规避 PostgreSQL 65535 参数上限。
- 修复（P0-3）：QPS 查询不再走 `qps_per_second` / `qps_per_5s` 视图（其谓词落在 `date_trunc(...)` 别名上，非 sargable，必然全表扫描），改为直接在 `requested_at` 上过滤并按秒/5 秒聚合。`EXPLAIN` 已验证走 `idx_requested_at` 索引扫描。视图本身也去掉了冗余 `ORDER BY`（P2-1）。
- 修复（P0-4）：刷盘循环的下限由 `.max(100µs)` 改为**最小 1 秒间隔**。DB 变慢导致一轮超过 1s 时直接跳过已错过的整秒（而非忙等重试），消除「越慢越快」的正反馈；超过 3s 记录 WARN。
- 修复（P0-5，**跨租户数据泄露**）：上游连接只有在响应体被完整读到 EOF 后才允许归还连接池。`StatisticsIncoming` 读到流结束会置位共享标志，连接任务据此决定归还或关闭；客户端中途断开、响应体被丢弃、上游出错、120s 超时都会**关闭**连接。`is_healthy()` 的 `write(&[])` 恒真检查已删除（改用零长度 `read` 探活，不消费数据）。已用真实网关 + 8MB 上游响应 + `curl --limit-rate` 强制中途断开验证：32/32 次后续请求内容完全正确、无串包。
- 修复（P0-6）：连接池上限真正生效。原先 `max_connections` 从未设置、保持 0，被解释为 `Semaphore::MAX_PERMITS`（等于无上限）。现在默认 `DEFAULT_MAX_CONNECTIONS = 256`，取许可带 5s 超时（饱和时快速失败而非无限排队）。**残留（诚实记录）**：连接由 hyper 的 `Connection` future 持有最长 120s，稳态下空闲复用率仍低，本次只解决「无上限」与「许可提前释放」，未实现真正的 keep-alive 归还。
- 修复（P0-7）：DDL 收敛到**单一迁移入口**并全程持有事务级 advisory lock；新增 `DbStartupMode::{Serve, Migrate, AutoMigrate}` 与 `--migrate` / `DB_AUTO_MIGRATE=1`。数据面默认 `Serve`（只校验、绝不执行 DDL，缺失表时明确报错并给出可操作提示），从根上消除两个进程并发重放 `CREATE TABLE/INDEX IF NOT EXISTS` 的启动竞态。触发器重建改为在**同一事务**内完成，消除 `DROP`/`CREATE` 之间的通知真空期；同时恢复周期性（10s）兜底全量同步作为最终一致性保障。
- 修复（P0-8）：站点/证书/监听器改为**对账式同步**。原先只 insert 不 delete，导致删除站点、移除域名、删除证书都不生效。现在每轮全量重建内存表（含预编译通配符），监听端口按集合 diff 后关闭多余监听。已用真实网关验证：改域名 → 旧域名立即 404、新域名立即生效；删站点 → 端口监听被回收（日志 `Stopped listening on port 18081`），重新加回后恢复监听。
- 修复（P0-9）：`update_updated_at()` 触发器改用 `clock_timestamp()`。原用 `NOW()`（事务开始时间），长事务提交后 `updated_at` 会早于网关已推进的同步水位，`WHERE updated_at > $1` 永久匹配不到该更新。改用对账式全量同步后该水位机制已不再依赖，但触发器语义仍一并修正。
- 修复（P0-10）：TLS `handshake_len` 增加硬上限 `MAX_TLS_HANDSHAKE_LENGTH = 64KB`（原可声明 16MB 并让网关真的分配等待），并新增 `ProtocolTLSError::HandshakeTooLarge`。预读路径（含 PROXY protocol 预读）整体加 10s 超时，且单连接预读总量有上限。
- 修复（P0-11 / P0-12，**前端放大器**）：`ky` 显式 `retry: 0`、`timeout: 15000`（默认会重试 2 次、5xx 时把一次轮询放大成三次全表聚合）；新增 `useVisiblePolling` 组合式函数，页面隐藏时暂停轮询、恢复时立即刷新一次，并对连续失败做指数退避（上限 60s）。QPS / metrics / bindTotp 三个轮询全部接入。
- 修复（P0-13，**HTTPS 中断**）：证书续签判定方向由 `expires_at < NOW() - '7 days'`（已过期超过 7 天）改为 `expires_at < NOW() + '7 days'`（7 天内到期或已过期），并新增 `idx_certificates_expires_at`（该查询每 30 秒执行一次，原先全表扫描）。
- 修复（P0-14）：证书签发改为**数据库侧跨进程互斥**：新增 `certificates.signing_started_at` 列，用 `SELECT ... FOR UPDATE SKIP LOCKED` + 显式事务抢占式认领，替代原先仅进程内的 `PENDINGS` 去重（多副本会重复消耗 ZeroSSL 配额并互相覆盖 fullchain/private_key）。认领释放放进 `Drop` 守卫，panic 路径同样释放（原实现 panic 后该证书永不再重试）。`certificate::init()` 现在返回真正的调度器 `JoinHandle`，`main.rs` 的 `abort()` 才能真正停止调度。
- 修复（P1-1）：移除 release 构建下 `access_map` 恒返回空对象的短路，生产环境访问地图恢复有数据；新增 `(remote_addr, requested_at)` 复合索引支撑该聚合。
- 修复（P1-4）：JWT `exp` 与 `exp_at` 单位统一为**秒**。原先 `exp` 用秒、`exp_at` 用分钟，令牌实际 7 天有效却告诉客户端 420 天。
- 修复（P1-5）：上游路径拼接不再有可被远端触发的 panic（authority-form 请求的 `uri.path()` 为空串，`&""[1..]` 会越界），也不再丢弃 base path —— 原 `Url::join` 会把 `http://h/api` + `foo/bar` 拼成 `http://h/foo/bar`。普通转发与 WebSocket 转发两处统一为显式前缀拼接。
- 修复（P1-6）：证书通配符正则改为**预编译**并随证书一起存储（原先每次 TLS 握手都要为每个候选模式重新 `Regex::new`），与 `sync/websites.rs` 的做法对齐；缓存加锁也改用 `unwrap_or_else(into_inner)`，poison 不再 panic。
- 修复（P1-7）：连接池取连接时不再**持锁 await** 健康检查（原先所有并发取连接在同一把 mutex 上排队，健康检查最长 100ms）。改为出锁后再探活。
- 修复（P1-8）：监听端口改为「**先 bind 成功再登记**」，bind 失败返回明确错误而不是在任务内 panic 后仍标记端口已监听；消除 `contains_key`→`spawn`→`insert` 的重复 bind 窗口（`SO_REUSEPORT` 下两次 bind 都会成功，连接被内核分摊到两个 accept 循环）。
- 修复（P1-9）：`/access/qps` 的 `count` 参数加 1..=300 校验，避免 `count=100000000` 单请求放大数据库负载。
- 修复（P1-15，IDOR）：`/auth/info` 不再允许任意已认证用户读取他人信息 —— 仅允许读自己，读他人需 admin。
- 新增（P1-16，**权限模型**）：`users` 表新增 `role` 列（CHECK 约束 `admin`/`user`/`view`，默认 `view`），新增 `Authorizer` 提取器与 `require_write()` / `require_admin()`。规则：`view` 只读、`user` 可创建/修改、`admin` 可删除与管理账号。`/auth/users`（账号枚举）与 `/auth/users/role`（改角色）限 admin；创建站点/证书/DNS 提供商需 `user` 及以上。已存在用户迁移为 `admin` 以保持现有能力不变。新增 `POST /auth/users/role`。
- 修复（P1-13，前端）：访问地图响应不再被 `console.log` 丢弃，改为赋给响应式状态并转换成 ECharts 需要的 `{name,value}[]`；死 watcher 接上 `in_days`；`Statistics.vue` 补传 `:in_days`。
- 修复（P2-2 / P1-11 / P2-10，前端）：metrics 轮询失败不再静默永久停止（改为退避重试且无未处理 rejection）；日期切换加 250ms 防抖 + `AbortController` + 请求序号守卫，杜绝慢响应乱序覆盖；`bindTotp` 绑定成功后停止 300s 轮询（原先会每 5 分钟申请一个新 TOTP 密钥）。
- 修复（P2-10 相邻）：`Constant.ts` 的 GET 请求在 `throwHttpErrors: false` 下 5xx 会"正常 resolve"，因此统计类接口显式在非 200 时抛错，否则退避永远不会触发。
- 新增（顺带修复，非审计条目）：站点匹配前**去掉 Host 头里的端口**。原先 `Host: example.com:8080` 无法匹配只配了 `example.com` 的站点，监听非 80/443 端口时所有请求都返回 404（多端口监听等于完全失效）。支持 `example.com:8080` 与 `[::1]:8080`，并补了单元测试。
- 变更（工程化）：`shared` 新增 `DbStartupMode` / `locks` / `Database::with_ddl_lock`；`initialize_access_logs` / `initialize_certificates` / `initialize_websites` / `initialize_dns_provider` 改为在调用方事务中执行；dashboard 的 `users` / `web_log` 迁移使用同一把 advisory lock。
- 修复（顺带）：`dashboard` 与 `gateway` 的 `init_config()` 改为**幂等**。原先 `CONFIG.set(..).unwrap()` 在重复调用时 panic（测试、以及未来可能的配置重载都会踩到）。
- 新增（测试）：`dashboard/backend/src/database/auth.rs` 增加授权链路集成测试（`authorization_chain`），覆盖：真实 `sign_jwt` 签发 → 解令牌 → 查库取 role → `require_write()`/`require_admin()` 判定；`Role::parse` 对非法值降级为 `view`；`Authorizer` 提取器从 `Authorization: Bearer` 解析角色并拒绝缺失令牌；**handler 级**断言 —— `view` 调 `POST /websites/create` 返回 403、`user` 返回 200、`view`/`user` 调 `/auth/users` 返回 403 而 `admin` 返回 200。未设置 `DATABASE_URL` 时自动跳过，不影响无数据库的 CI。
- 变更：`gateway/src/upstream/connection.rs` 新增 `PooledUpstreamConnection::drained_flag()` / `take_parts()`，`available_permits()` 取代有误导性的 `max_connections()`。
- 验证：`cargo check` / `cargo test` / `cargo fmt --check` / `cargo clippy`（改动文件零新增告警）全部通过；新增 2 个测试（`strip_port` 单元测试 + 授权链路集成测试，后者含 handler 级 403/200 断言）。另外在真实 PostgreSQL 18.2 + 真实网关进程上做了端到端验证：迁移幂等、QPS 查询走索引、访问日志（请求/响应/size/大小更新）正确落库且不重复、配置改动与删除实时生效、监听端口按需回收、TLS SNI 证书选择与通配符匹配正确、中途断开响应不串包。验证用的临时脚本与测试数据已清理。
- 已知残留（本轮未做，明确记录以免误解）：① 上游连接池的空闲复用率仍然低（见 P0-6 说明）；② 访问日志表无分区/无 TTL，QPS 查询已走索引但表仍会无限增长（属 [CONVERSATION.md](CONVERSATION.md) 迁移方案范围）；③ 网关仍是单点，未做多后端负载均衡（只用 `backends.first()`）与 PROXY protocol 解析；④ 前端 `eslint` 仍有 102 个**存量**错误（改动文件零新增），且 `eslint.config.ts` 依赖未声明的 `jiti`、未忽略 `dist`，建议后续单独修。

### 更早的未发布条目

- 新增：`crates/acme` 证书构造模块 —— `CertificateBuilder`（域名收集 + 构建）与 `Certificate`（基于域名的 SHA-256 `hash_id`），见 `crates/acme/src/lib.rs`、`crates/acme/src/util.rs`。
- 新增：`crates/acme` 加入 workspace members，依赖 `acmex`（features: `dns-tencent`、`zerossl-ca`）、`hex`、`rustls-acme`、`sha2`。
- 变更：`.gitignore` 增加 `tests/*`。
- 变更：`Cargo.lock` 随 `acme` 依赖更新。
- 修复：`gateway` 请求 URI 处理保留查询参数；访问日志与错误日志规范化（提交 `64cbdad`、`5a38845`、`0900f63`）。
- 已知问题：`crates/acme/src/main.rs` 使用硬编码绝对路径 `include_str!("/develop_workspaces/...")`，不可移植。
- 已知问题：`CertificateBuilder::private_key` 中残留 `println!` 调试输出；`public_key` / `fullchain_key` 字段尚未使用。
- 说明：以上「新增 / 变更」为工作区当前未提交改动，提交前请确认范围。

## [2026-10-02]

### 上午

- AI 修改：新增项目总览文档 [agent.md](agent.md)（项目概述、技术栈、目录结构、模块职责、关键入口与常用命令）。
- AI 修改：新增待办清单 [TODO.md](TODO.md)，包含从代码中提取的未完成项与「待确认」事项。
- AI 修改：新增本变更日志 [CHANGELOG.md](CHANGELOG.md)。
- AI 修改：文档内容基于代码扫描生成，未改动任何业务代码。
- 新增：无。
- 修复：无。

### 下午（问题审计）

- AI 修改：新增审计报告 [ISSUES.md](ISSUES.md)，逐行核对代码定位 **14 个 P0（事故级）**、**16 个 P1（严重）**、**12 个 P2** 问题，每条附 `文件:行号` 证据与修复方向。
- AI 修改：在 [agent.md](agent.md) 增加「已知风险提示（给后续 AI）」小节，避免后续改动踩到已定位的坑。
- AI 修改：在 [TODO.md](TODO.md) 增加「缺陷修复」清单，按 P0/P1 拆分为可勾选条目。
- 已知问题：数据库侧确认「多进程争抢同一批表」真实存在，且构成完整雪崩链路 —— QPS 视图全表扫描（无分区/无清理）+ 面板 5s 轮询 + 网关刷盘循环在 DB 变慢时退化为 100µs 空转（正反馈）。
- 已知问题：访问日志存在**真实数据丢失**路径 —— 刷盘"先删内存后写库"（写失败即永久丢失），且批量 INSERT 未分块会撞 PostgreSQL 65535 参数上限（>6553 请求/秒必然失败）。
- 已知问题：上游连接池会把**未读完响应的连接**放回池中复用，可导致响应串包与跨站点数据泄露（安全事故级）。
- 已知问题：SNI 预读无长度上限，单连接可申请 16MB（慢速攻击可耗尽内存）。
- 已知问题：站点/证书删除后不从内存移除，已下线域名与已吊销证书**继续生效**；`NOW()` 水位导致更新可能永久丢失。
- 已知问题：**证书续签判定方向写反**（`expires_at < NOW() - 7 days`），证书在到期后才开始续签，会导致站点 HTTPS 中断；且多副本部署时无跨进程互斥，会重复消耗 ACME 配额。
- 已知问题：前端 ky 默认重试在 5xx 时把统计请求放大 3 倍，且所有轮询无 `visibilitychange` 门控 —— 二者是数据库压力的重要放大器。
- 已知问题（安全）：`/auth/info` 存在越权读取（IDOR），任意已认证用户可枚举他人账号；系统**完全没有角色/权限模型**，任一登录账号即等同管理员，可读取全部 DNS 服务商凭据。
- AI 修改：归纳并交叉核对外部 AI 产出的《访问日志存储优化 + v1→v2 迁移方案》[CONVERSATION.md](CONVERSATION.md)，核对结论追加为 [ISSUES.md](ISSUES.md) 的「附：交叉核对」章节，并在 [agent.md](agent.md) 登记该文档。
- 待评审：该迁移方案已覆盖 P0-3 / P0-7 / P1-1 / P1-10，但**未覆盖** P0-1（丢数据）、P0-2（分块）、P0-4（循环自激）、P0-13（证书续签）与前端放大器 P0-11 / P0-12；另识别出 6 处技术风险（移除外键会失去孤儿行保护、`get_access_info` 响应侧无法分区裁剪、回滚步骤不含索引与 FK 恢复、`DatabaseQPS` 字段描述与代码不符、PG 版本无法从仓库验证、时区一致性成为硬要求）。
- 修复：无（本次仅审计与文档，未改动业务代码）。

## [初始版本] - 2026-01-05

仓库基线，截至提交 `64cbdad`（共 185 次提交）。项目处于开发中，尚未发布正式版本。

- 新增：**数据面网关** `gateway` —— 基于 hyper + tokio-rustls 的 HTTP(S) 反向代理，支持 TLS SNI 路由、上游连接池、WebSocket 升级转发、访问日志采集。
- 新增：**前置协议解析** `crates/protocols` —— TLS ClientHello / SNI 提取，PROXY protocol v1、v2（骨架）。
- 新增：**共享库** `crates/shared` —— PostgreSQL 连接与初始化（`uint128`、`btree_gin` 扩展 + NOTIFY 触发器）、站点/证书/访问日志/DNS 提供商数据模型、双栈监听器、日志组件、favicon 抓取。
- 新增：**低依赖共享库** `crates/simple_shared` —— `ObjectId`、mnt 通信协议、zigzag varint 编解码、版本信息（由 `build.rs` 从 `project.toml` 注入）。
- 新增：**DNS 服务商封装** `crates/dnsprovider` —— 抽象层与 DNSPod（腾讯云）实现。
- 新增：**控制面 API** `dashboard/backend`（axum）—— JWT 鉴权、TOTP 绑定与验证、站点/证书/DNS 提供商/日志/访问统计路由、GeoIP 归属查询（MaxMind MMDB + CZ88）、Unix Socket 运维通道。
- 新增：**控制面前端** `dashboard/frontend`（Vue 3 + TypeScript + Vite）—— 登录、站点管理、证书管理、DNS 提供商管理、访问统计（QPS、访问地图、日志）。
- 新增：**运维 CLI** `mnt` —— 通过 `/tmp/webgateway-mnt.sock` 与后端通信（获取管理员 TOTP）。
- 新增：**IP 库工具** `ipdb` —— 从 GitHub 拉取 MaxMind GeoLite2、ip2region、CZ88 数据库并输出到 `dashboard/backend/assets/ipdb/`。
- 新增：**网关错误页** `assets/error_pages/`（Vite + TypeScript 静态站点）。
- 新增：**部署与 CI** —— `docker-compose.yml`（postgres / gateway / dashboard-backend / dashboard-frontend）、`.env.default`、`gateway` 与 `dashboard/*` 的 Dockerfile、GitHub Actions 构建并推送镜像到 GHCR。
- 已知问题：`gateway/src/proxy/` 旧代理实现已停用，与 `gateway/src/upstream.rs` 功能重叠。
- 已知问题：PROXY protocol 解析、站点多后端负载均衡、favicon 抓取均未实现。
- 已知问题：项目无自动化测试。
