# Changelog

本项目所有值得记录的变更都写入本文件。版本号取自 [project.toml](project.toml)（当前 `0.1.0`）。
条目前缀统一使用：**AI 修改** / **新增** / **变更** / **修复** / **移除**，标注「AI 修改」的条目由 AI 助手生成或变更。

## [未发布]

### 2026-10-02（第九轮：证书过期过滤 + 站点编辑/删除，07:45:00Z — 07:58:41Z）

本轮实施 [STEP.md](STEP.md) 新版「目前」的第 2、3 项。第 1、4 项（统计表 v2 + 按周分区 +
面板加速）是**大表结构迁移**，需要停机窗口，方案与风险见
[PRODUCTION_READINESS.md](PRODUCTION_READINESS.md) 第十节，确认后再动。所有时间戳为 UTC。

- 新增（07:50:00Z，目前-2）：**过期的证书不再加载**，除非一张有效的都没有。
  `gateway/src/sync/cert.rs` 的 `sync_certificates` 现在先解析全部证书并标注是否过期，再按规则装载：
  有未过期的就只装载未过期的；全部过期则全部装载（宁可继续用过期证书，也不要无证书可用），
  并打 ERROR 提示续签。规则抽成 `loadable_certificates()` 并加 3 个单测
  （有有效证书时跳过过期 / 全过期时回退全部 / 空列表）。
  背景：生产库现有 2 张通配符证书，其中 `6a2b8ef3…` 已于 2026-09-04 过期，此前仍会被加载用于 TLS。
- 新增（07:55:00Z，目前-3）：**面板支持编辑与删除站点**。
  - 后端：`DatabaseWebsiteModifyRepository` 增加 `update_website` / `delete_website`；
    路由新增 `GET /websites/{id}`、`POST /websites/{id}/update`（需 `user` 及以上）、
    `POST /websites/{id}/delete`（**需 admin** —— 删除会让该站点所有域名立即停止服务）。
    影响行数为 0 时明确回 **404**，而不是假装成功（否则前端会刷新出"看起来改了但没生效"的列表）。
  - 更新语义是**整条覆盖**而非合并：面板提交完整表单，合并语义会让"删掉某个域名/后端"无法生效。
  - 前端：`AddWebsite.vue` 复用为编辑对话框（传 `website` 即编辑模式、表单预填、提交走 update，
    并新增"网站名称"输入框）；网站卡片增加「编辑」「删除」按钮，删除前弹确认框。
  - 布局：网站列表改为**每行三个**（`flex: 0 0 calc((100% - 32px) / 3)`，窄屏退化两列/单列）；
    搜索框接上过滤（原先只有个不生效的输入框），并补了空状态提示。
  - 验证：真实库跑通 CRUD —— 创建 → 更新后旧域名消失（覆盖语义生效）→ 读取一致 →
    更新不存在的 id 返回 0 行 → 删除 1 行 → 重复删除 0 行；前端 `vue-tsc -b` 与 `pnpm build` 通过。
- **未做（需要你的决定）**：目前-1（统计表 v2 + 按周轮换）与目前-4（迁移后做查询优化）。
  这两项要在 **1380 万行 / 约 6.3 GB** 的表上做**分区化重写**（移除外键、主键加分区键、
  数据搬迁、读写切换），必须规划停机窗口。刚发生过启动路径迁移导致停机的事故，这次先出方案再动手。

### 2026-10-02（第八轮：修复上线事故 —— 启动路径的迁移在 1600 万行表上挂死，07:14:00Z — 07:26:00Z）

**这是一次真实的生产事故，责任在我（第七轮的自动迁移设计）。** 用户部署后 gateway 与
dashboard 全部不可用：两个进程都卡在 `pg_advisory_xact_lock(SCHEMA_INIT)` 上，
容器被健康检查判定为"起不来"并重启，重启又让前一个迁移**整体回滚、重来一遍** —— 死循环。

- 事故根因（07:14Z 现场证据）：`migrate_size_logs_second_granularity` 把
  **全表回填 + 合并 417 万重复行 + 建 1600 万行唯一索引** 放在了**启动路径**的单个事务里，
  全程持有 `SCHEMA_INIT` 排他锁。生产日志显示 `SELECT pg_advisory_xact_lock($1)` 阻塞
  **279 秒**仍不返回；另一个进程只能干等；随后容器重启导致
  `terminating connection due to administrator command`、事务回滚，循环往复。
- 修复（07:24Z，**把迁移代价降到与表大小无关**）：`at_second` 改为**可空列**
  （PostgreSQL 11+ 加可空列是元数据操作，**不重写表**），唯一索引改为**部分唯一索引**
  `WHERE at_second IS NOT NULL` —— 只约束**新写入**的行，老行被谓词直接排除，
  因此**无需回填、无需去重、无需扫全表**。
  写入侧改为
  `ON CONFLICT (xxx_id, at_second) WHERE at_second IS NOT NULL DO UPDATE ...`：
  谓词必须与索引完全一致，否则 PostgreSQL 推断不出该索引并报
  `there is no unique or exclusion constraint matching the ON CONFLICT specification`
  （已实测；顺带实测了 `date_trunc('second', created_at)` **不能**建索引 ——
  它不是 IMMUTABLE，所以只能落成实体列）。
  实测迁移耗时：**356 ms**（修复前在同一库上需要数分钟）。
  历史行原样保留（`at_second` 为 NULL），其秒级信息本来就在 `created_at` 上；
  保留期清理的窗口谓词用 `created_at`，对新老行都有效。
- 修复（07:25Z，**防呆**）：迁移取 `SCHEMA_INIT` 锁改为**限时 60 秒**
  （事务内 `SET LOCAL lock_timeout`）。超时后快速失败并给出可操作提示，而不是静默挂死、
  被编排反复重启。原来的失败模式在日志里只有一行 `rows_affected=0` 的慢查询，极难定位。
- 验证：`cargo check --workspace --all-targets` 零错误、`cargo test --workspace` 全绿；
  在本机把 beta 库还原成"生产当前状态"（无 `at_second`、无唯一索引）后重跑迁移，
  耗时 **356 ms**；新写入仍按 `(请求, 秒)` 累加（同一秒多次上报 → 1 行、跨秒 → 2 行）。
- 修复（07:35Z，**兼容生产已建成的索引**）：size 表的累加写入改为
  **`DELETE 该键已有行（RETURNING 取旧值） → INSERT 旧值+增量`**，不再使用
  `INSERT ... ON CONFLICT DO UPDATE`。
  原因：累加语义依赖"冲突目标能解析到唯一索引"，而**两种索引语法互不兼容** ——
  全量索引 `(xxx_id, at_second)` 认 `ON CONFLICT (xxx_id, at_second)`，但认不出带谓词的版本；
  部分索引 `WHERE at_second IS NOT NULL` 则必须带同样谓词才行。
  生产库在本次事故中**已经把全量唯一索引建成**（旧版迁移跑完了），而仓库新方案建的是部分索引，
  若代码只认一种，**下一次部署就会让响应大小刷盘整批失败**。
  `DELETE + INSERT` 不依赖冲突推断，对两种索引都成立（已实测）。
  并发安全：`DELETE` 会锁住该键已有行，并发事务要么在其 `DELETE` 中取回我们的最终值继续累加，
  要么被行锁阻塞到提交，不会丢增量。批次内同键行在内存先合并（同一原因）。
  验证：同秒 4 次 + 跨秒 1 次 + 再跨批次同秒 1 次 → **2 行 / 字节和 187**，秒键正确。
- 修复（07:40Z，**生产实测到的第二个故障**）：size 表累加写入增加**父行守卫**。
  生产日志出现
  `Failed to flush access response size inserts (2 rows); ... violates foreign key constraint
  "access_response_size_logs_response_id_fkey"` —— size 表对主表有外键，当父行不在
  （未落库/已被清理）时 INSERT 直接抛错；而累加批次是"失败就整批留内存重试"的语义，
  于是**一个坏行会把整批永久卡住**，响应大小统计停止写入（与 P0-1 同类的毒化模式）。
  改为 `INSERT ... SELECT ... WHERE EXISTS (SELECT 1 FROM 主表 WHERE id = $2)`：
  父行缺失的行**静默跳过**（没有主表行就没有统计对象），不再拖垮整批。
  验证：正常行与"父行缺失"行放在同一批 → 整批成功、正常行写入、缺失行跳过。
- 教训（已写入 [agent.md](agent.md)）：**启动路径上的迁移必须是与表大小无关的廉价操作**
  （建表/加可空列/建部分索引）。任何需要回填、去重、在大表上建全量索引的动作，
  都必须放到服务启动之后的**后台任务**里分批执行。第七轮的设计违反了这条，
  代价是生产停机约 12 分钟。

### 2026-10-02（第七轮：size 表按秒颗粒度 + 无感自动迁移，06:24:00Z — 06:46:03Z）

本轮按 [STEP.md](STEP.md) 更新后的「目前」两节实施。**注意：第七轮修正了第六轮的一个设计缺陷**
—— 第六轮把响应大小按请求聚合成一行，**丢掉了秒级颗粒度**，而这两张表的用途正是"细化到每秒"。
所有时间戳为 UTC。

- 修复（06:40:00Z，**修正第六轮的过度聚合**）：size 明细表改为按 **`(请求, 秒)`** 聚合。
  - 第六轮的做法是「同一请求的多个 chunk 合并成一行」，虽然消除了 7 倍膨胀，但**跨秒也合并了**，
    秒级颗粒度彻底丢失。
  - 现在：新增 `at_second`（截断到整秒）列，唯一键 `(xxx_id, at_second)`；
    gateway 侧 `SizeAccumulator` 按 `(id, 秒)` 在内存累加，落库用
    `ON CONFLICT (xxx_id, at_second) DO UPDATE SET body_length = body_length + EXCLUDED.body_length`。
    **同一秒内合并、跨秒分开** —— 行数按"请求数 × 实际跨秒数"收敛（单秒内完成的请求只占一行），
    同时保住每秒颗粒度。
  - 老库升级：迁移会补 `at_second`、用 `created_at` 回填、**合并同秒重复行**（保留行取组内
    `MIN(id)`，字节数取组内 `SUM`，因此历史统计**不会变小**）、再建唯一索引。
    生产库实测有 **298541 组重复、约 417 万行需要合并**，因此**首次启动会明显变慢**（一次性代价）。
  - 新增 3 个测试：`response_size_chunks_accumulate_per_second`（同秒合并且跨秒分开）、
    `accumulator_rollback_adds_instead_of_overwriting`、`merge_same_second_folds_duplicate_keys`。
- 修复（06:42:00Z，**迁移期的真实约束**）：单条 `INSERT ... ON CONFLICT DO UPDATE` **不允许两次命中
  同一冲突键**（PostgreSQL 报 `ON CONFLICT DO UPDATE command cannot affect row a second time`，
  已实测）。因此落库前先用 `merge_same_second` 在内存合并批次内的同键行；并改为**整批单事务**
  —— 累加写不是幂等的，若分块提交，"第一块成功、第二块失败后整批重试"会**重复累加**字节数。
- 新增（06:45:00Z，**无感自动迁移**）：`DbStartupMode::from_env_args` 的默认值由 `Serve` 改为
  **`AutoMigrate`** —— 任一服务启动时都会在 `locks::SCHEMA_INIT` 事务级 advisory lock 下补齐完整
  schema 再进入服务，因此 gateway 与 dashboard **谁先启动都一样**，全新库直接 `up` 即可。
  早期默认 `Serve` 是因为担心两进程并发重放 DDL（ISSUES.md P0-7 问题 A），而该竞态已由
  advisory lock + 幂等 DDL 消除。保留逃生开关：`DB_AUTO_MIGRATE=0`（或 `false`）＝只校验不建表。
- 修复（06:45:00Z，自动迁移的前提）：**控制面的表（`users` / `users_client_secrets` / `web_log`）
  原本只在 dashboard 的迁移里创建**，gateway 无法调用 —— 于是"gateway 先启动"时库中缺 `users`，
  而 gateway 的 `verify_database_schema` 恰恰要求它存在，所谓"自动迁移"名不副实。
  现把这三张表的 DDL 收敛到 `crates/shared/src/database/dashboard_schema.rs` 并纳入共享迁移入口；
  dashboard 的 `init_authentication` / `initialize_web_log_tx` 改为转调共享实现（不再保留第二份 SQL）。
  `verify_database_schema` 的必需表清单同步加入 `users_client_secrets`、`web_log`。
- 变更（06:45:00Z）：`docker-compose.yml` 去掉一次性 `migrate` 服务（不再需要），
  两个服务各自显式 `DB_AUTO_MIGRATE: "1"` 并 `depends_on: postgres`。
- 验证：
  - `cargo check --workspace --all-targets` 零错误；`cargo test --workspace` 全绿
    （gateway 3 个、shared 6 个等）；
  - 真实库端到端：同一秒 3 次上报 → **1 行 / 字节和 132**；跨一秒再上报 → **2 行**（秒级颗粒度保留）；
  - 迁移后在真实库确认：`at_second` 列、两个 `uniq_access_*_second` 唯一索引、`users_info.role`、
    `configurations` 表与 `access_log_retention` 配置全部就位；
  - 两个二进制默认启动均为 `Database startup mode: AutoMigrate`；`DB_AUTO_MIGRATE=0` 时为 `Serve`。
- 说明（数据安全）：生产库**仍为只读**，未执行迁移。上面的重复行统计是只读查询得出的；
  生产迁移要等部署新镜像时由服务自动完成（建议低峰期，首次会合并约 417 万行）。

### 2026-10-02（第六轮：访问日志保留期 + 修复 size 表存储膨胀，06:02:00Z — 06:23:58Z）

本轮按 [STEP.md](STEP.md) 更新后的「目前」一节实施：① 证书续签暂不处理（用户明确要求）；
② 压缩/清理数据库历史数据（可在控制面板配置，下限 3 个月）；③ 控制面板可选配置 HTTP/HTTPS
代理以便续签证书（**经核实无需改代码**，见下）。所有时间戳为 UTC。

- 修复（06:14:00Z，**存储膨胀 7 倍，本轮最重要的发现**）：**响应体大小明细表按 body chunk
  逐块插行，导致 `access_response_size_logs` 达到 1600 万行 / 3.6 GB（请求主表的 7 倍）。**
  `StatisticsIncoming::poll_frame` 每读到一个 chunk 就调用一次
  `insert_increase_response_size_log`，而该函数原来会**为每次调用新生成一个 ObjectId 并插一行**
  —— 一个响应体被分成 N 块读取就写 N 行。
  生产库实测：`access_response_size_logs` 16036486 行 vs `access_request_logs` 2280559 行
  （比值 **7.03**）；近 24 小时仍在以 **6.40 倍**的重复率增长；单条 `response_id`
  最多挂了 **262791 行**（2026-07-03 起 4 天内）。
  修法：gateway 侧新增 `SizeAccumulator`，把同一 `id` 的多个 chunk **在内存里累加**，
  每轮刷盘每个 id 只落一行（`body_length` 为累加值，`created_at` 取首次出现时间以保证与主表
  时间轴一致、便于保留期裁剪）。刷盘失败时回滚是**相加**而非覆盖，避免丢掉刷盘期间新到的字节。
  新增 2 个回归测试锁住该行为。
- 新增（06:16:00Z，控制面板可配，下限 3 个月）：**访问日志保留期 + 自动清理**。
  - 配置存放于 `configurations` 表（key = `access_log_retention`），字段
    `retention_days`（默认 **180 天**，夹紧到 **[90, 3650]**）与 `enabled`（默认开启）。
  - 清理由数据面 gateway 每小时执行一次，按「先删两张 size 明细 → 再删响应 → 最后删请求」
    的顺序（严格遵守外键依赖），每轮最多 5 万行、最多 20 轮，用**小事务**推进避免长事务持锁
    与 WAL 暴涨。
  - 多实例并发安全：用 `pg_try_advisory_lock(locks::RETENTION)` 独占；**全程占用同一条连接**
    取锁/删数据/解锁 —— 会话级 advisory lock 若跨连接获取与释放会静默失效（本轮修掉的自伤点）。
  - 新增 API：`GET /settings/retention`、`POST /settings/retention`（需 `user` 及以上）、
    `POST /settings/retention/prune`（立即清理一轮，最多 25 万行）。
  - 前端「设置 → 数据保留」页：可改保留天数、开关自动清理、手动触发一轮清理；
    保存后用**后端返回的实际生效值**回显，避免"设了 30 天但实际 90 天"的静默偏差。
- 修复（06:16:00Z）：**`configurations` 表从未被创建**。`initialize_configuration` 原先在连接池上
  直接执行 DDL、且**从未被任何地方调用**，导致这张表在真实库里根本不存在（生产库实测
  `relation "configurations" does not exist`）。现改为与其他 initializer 一致的事务签名
  （`&mut Transaction`）、接入 `inner_init_database_with`，并加入 `verify_database_schema`
  的必需表清单。
- 变更（06:16:00Z）：迁移时以 `ON CONFLICT (key) DO NOTHING` 写入保留期默认值（180 天），
  因此重复迁移**不会**覆盖运维已经调整过的值。
- 核实（06:20:00Z，任务 3「面板可选配置 HTTP/HTTPS 代理以续签证书」）：
  **无需改代码即可支持**。acmex 用 `reqwest::Client::builder()` 构造客户端且**没有**调用
  `.no_proxy()`，reqwest 默认读取 `HTTP_PROXY` / `HTTPS_PROXY` / `ALL_PROXY` / `NO_PROXY`
  环境变量，因此给 dashboard-backend 容器设置 `HTTPS_PROXY` 即可让 ACME 签发走代理。
  本轮未加面板字段（用户在 STEP.md 中已把证书续签标为"先不用管"），已记录到 [TODO.md](TODO.md)。
- 验证：`cargo check --workspace --all-targets` 零错误；`cargo test --workspace` 全绿
  （新增 2 个 gateway 单测 + 2 个保留期夹紧单测）；前端 `pnpm build` 通过；
  真实库验证保留期配置读写（设 10 天 → 实际生效 90 天）、清理外键顺序
  （造 200 天前的请求 + 响应 + 两张 size 明细，清理后过期行 0、未过期行保留 1、孤儿行 0）；
  生产库只读确认 `configurations` 表缺失（印证上面那条修复的必要性）。
- 说明（数据安全）：清理函数只在 beta 库上执行过，且 beta 库现有数据全部落在保留期内
  （最早 2026-08-17），实测未删除任何真实数据（仅删掉自造探针数据）。
  生产库**全程只读**，未执行任何迁移或删除。生产库里 `host='probe'` 的 13 行是历史测试数据
  （2026-05-20 起），与本次工作无关。

### 2026-10-02（第五轮：release 构建解阻 + pnpm 迁移 + 生产只读排查，05:52:00Z — 06:01:33Z）

本轮处理 [STEP.md](STEP.md) 更新后的内容：解决 release 构建阻塞、前端 yarn→pnpm 迁移、
生产环境 SSH 只读排查、并把 STEP.md 融合进 [agent.md](agent.md)。所有时间戳为 UTC。

- 修复（05:57:00Z，**release 构建阻塞，第四轮遗留**）：**vendor `acmex` 0.8.0 并去掉硬编码的 `fips` feature。**
  上游 `acmex/Cargo.toml` 把 `aws-lc-rs = { features = ["fips"] }` 写死且非可选，导致任何下游都必须编译
  FIPS 版后端 `aws-lc-fips-sys`；其 `bcm-delocated.S` 会被较新的 binutils 拒绝
  （`error while processing "\t.section\t.data.rel.ro.local...": ".data section found in module"`），
  整个 workspace 因此产不出 release 产物（退出码 101）。
  实测：升级 `aws-lc-fips-sys` 0.13.14→0.13.17 **无效**；而关掉 fips 后用的 `aws-lc-sys` 在本机
  38 秒即编译通过。acmex 全仓**只有一处**用到 aws-lc-rs（`src/storage/encrypted.rs` 的 AES-256-GCM），
  与 FIPS 认证无关，故去掉该 feature 功能完全等价。
  做法：`vendor/acmex/`（569 KB，只保留 `src/`、`Cargo.toml`、两个 LICENSE 与 README；
  上游的 docs / examples / tests / .github 已剔除，不影响构建），并在根 `Cargo.toml` 用
  `[patch.crates-io] acmex = { path = "vendor/acmex" }` 重定向，补丁处写明原因；
  `dashboard/backend/Cargo.toml` 仍写 crates.io 版本号，便于上游修复后一行回退。
- 验证（05:58:00Z）：`cargo build --release --bin dashboard --bin gateway --bin webgateway-mnt`
  **退出码 0**，产出 `target/release/{dashboard,gateway,webgateway-mnt}`；
  `cargo tree -p dashboard` 中 `aws-lc-fips-sys` 计数为 **0**；
  两个 release 二进制对真实库跑 `--migrate` 均退出 0。
- 变更（05:58:00Z，可选任务）：**前端包管理器 yarn → pnpm**。新增 `dashboard/frontend/pnpm-lock.yaml`
  （唯一锁文件）与 `pnpm-workspace.yaml`；删除 `yarn.lock`、`.yarnrc.yml`、`.yarn/install-state.gz`
  （后者是误提交的二进制产物）。`package.json` 增加 `packageManager: pnpm@12.8.1`、
  `typecheck` / `lint` 脚本，并把 yarn 的 `resolutions` 迁移为 pnpm `overrides`
  （pnpm 12 起该设置位于 `pnpm-workspace.yaml`，`package.json` 的 `pnpm` 字段已不再被读取）。
  CI 的 [.github/actions/build-frontend/action.yml](.github/actions/build-frontend/action.yml)
  改用 `pnpm/action-setup@v4` + `pnpm install --frozen-lockfile` + `pnpm build`。
  验证：`pnpm install --frozen-lockfile` 通过、`pnpm build` 通过（`vue-tsc -b` + `vite build`）。
- 移除（05:58:00Z）：仓库根目录的 `package.json` + `yarn.lock`。它们是历史上 `qrcode-vue3`
  的残留（该依赖已被 `vue3-next-qrcode` 取代），全仓无任何引用，也不参与任何构建。
- 修复（05:59:00Z）：`.gitignore` 增加 `.pnpm-store`（pnpm store 固定在 `target/pnpm-store`，
  避免写家目录 —— 本开发环境家目录只读）。
- 排查（06:00:00Z，**只读，未做任何变更**）：SSH 上生产 `10.240.0.1` 核对现状，要点见
  [agent.md](agent.md)「生产部署现状」与 [PRODUCTION_READINESS.md](PRODUCTION_READINESS.md)：
  - 生产库真实库名是 **`postgres`**（STEP.md 里写的 `webgateway` 不存在）；生产库**缺
    `users.role` 与 `certificates.signing_started_at`**，所以新镜像上线前必须迁移。
  - 镜像来源是镜像站 **`ghcr.milu.moe`**（`IMAGE_PREFIX=ghcr.milu.moe/ttbnetwork`），
    不是 `ghcr.io`；部署目录 `/opt/webgateway/`。
  - 运行中的镜像是 3 周前构建、**早于全部审计修复**（日志无 `Database startup mode: Serve` 特征）；
    `/opt/webgateway/.env` 无 `DB_AUTO_MIGRATE`，compose 里也没有 migrate 服务。
  - **生产日志实证了本轮修复的必要性**：46 万行日志中
    `Failed to send request: operation was canceled` 出现 **8099 次**，
    `hyper::Error(Canceled, IncompleteMessage)` 7897 次 —— 即用户报告的 `operation was canceled`。
  - 旧镜像仍在跑 `qps_per_5s` 视图：dashboard 日志显示单次查询 **4.66 秒**（阈值 1s），
    且前端每 5 秒轮询一次，与 ISSUES.md P0-3 的判断一致。
  - TLS 握手失败占日志主体（`BadCertificate` 25.7 万次、`tls handshake eof` 10.4 万次、
    `NoCipherSuitesInCommon` 6.6 万次），多数为扫描器探测，建议单独排查。
  - 主机规格：Debian 13 / 2 vCPU / **1.9 GiB 内存无 swap** / 根分区 39G 已用 50%。
- 变更（06:01:33Z）：`STEP.md` 的「需要修改 / 可选修改 / 最后」三节融合进
  [agent.md](agent.md)（新增「任务单」与「生产部署现状」两节）；「授予权限」按约定
  **只保留非凭据事实**（测试域名、生产库/SSH 的存在与用途），连接串与口令不入库。
- 新增（06:01:33Z）：[agent-context.md](agent-context.md) —— 会话上下文与踩坑记录，
  便于下次接手（环境地址、生产缺失列、`tests/` 非测试目录、`shared` 无 tokio multi-thread、
  `TrySendError` 用法、后台进程必须用受管任务等）。

### 2026-10-02（第四轮：生产可用性修复，05:24:30Z — 05:45:18Z）

本轮处理 [STEP.md](STEP.md) 提出的三件事：① 保证能在生产环境直接在线迁移并运行；
② 修 gateway 的 `operation was canceled`；③ 把 `CONVERSATION.md` 合并进 `ISSUES.md`。
所有时间戳为 UTC。改动文件：`gateway/src/upstream.rs`、`gateway/src/upstream/connection.rs`、
`crates/shared/src/database.rs`、`crates/shared/src/database/access.rs`、
`crates/shared/src/database/certificate.rs`、`docker-compose.yml`。

- 修复（05:43:49Z，**P0-5 回归，最严重**）：**上游连接任务结束后连接被放回空闲池，导致后续请求永久挂起。**
  连接任务把 `poll_without_shutdown` 的 `Ready(Err(e))`（上游 EOF / 出错）当成"正常结束"，
  于是"响应体已读完"的标志被保留，这条已无 future 驱动的连接被 `Drop` 放回池中；
  下一个请求复用它时，`try_send_request` 把请求投递给**没有接收者的 dispatch 队列**并永久挂起
  —— 请求根本到不了上游，客户端只能等到超时。实测表现：**每隔一个请求超时**（`curl` 60 次 =
  30 成功 / 30 超时；`crates/shared` 探针同样 1 成功 1 超时）。
  修法：`PooledUpstreamConnection` 新增 `task_exited` 共享标志，连接任务在退出前**无条件**置位；
  `is_reusable()` 改为「响应体已读到 EOF **且** 连接任务仍在运行」。同时把连接出错日志
  从 `ERROR` 降为 `DEBUG`（对端正常关闭 keep-alive 不是错误，避免刷屏）。
  验证：修复前 6/6 中 3 次超时 → 修复后「每次新连接」与「单连接 keep-alive」两种模式各 6/6 全部 200，
  另跑 `curl` 50 次 50/50 成功。
- 修复（05:41:00Z）：`try_send_request` 重试——复用的 keep-alive 连接若已被上游关闭，
  hyper 会返回 `Kind::Canceled`（其 Display 正是用户看到的 `operation was canceled`）。
  改用 `try_send_request`（而非 `send_request`）以取回请求体，并按 hyper-util `legacy::Client`
  的判据**只对「取自空闲池 + 请求体被原样交还（一个字节都没发出）」的连接重试一次**；
  `PooledUpstreamConnection` 增加 `is_reused()` 供该判断使用。
  **注意**：这条只是把"偶发 502"变成"自动重试"，上面那条挂起才是 `operation was canceled` 的主因。
- 修复（05:31:00Z，**P0-1 残留**）：访问日志批量 INSERT 改为**幂等**（`ON CONFLICT (id) DO NOTHING`，
  两个 size 明细表用 `ON CONFLICT DO NOTHING`）。刷盘是「批量取出 → 写库 → 成功才清空内存」，
  而分块执行不是原子的：前几块提交成功、最后一块失败时，下一轮重试会命中主键冲突并**永久失败**，
  该类型日志的刷盘队列被彻底毒化（只进不出、内存无限增长直至 OOM）。
  验证：真实数据库上「整批重试」与「部分成功后整批重试」两个场景均通过。
- 修复（05:32:21Z，**P0-14 残留**）：证书签发认领增加**过期回收**（`STALE_SIGNING_CLAIM = 10 minutes`）。
  原先释放只靠 `SigningGuard::drop`，进程被 SIGKILL / OOM / `docker kill` 打断时不会执行，
  该证书永远停留在"已认领"状态、再也不续签，只能人工改库。
  验证：真实数据库三场景——1 秒前的认领不被抢占 ✓、30 分钟前的僵死认领被回收 ✓、回收后不会重复签发 ✓。
- 修复（05:32:21Z，**会导致自动续签 100% 失败**）：`try_claim_certificate_signing` 的 SELECT
  **漏选 `email` 列**，而 `NeedSignCertificate` 的 `FromRow` 要求该列 → 只要存在满足条件的证书，
  认领查询就必然报 `no column found for name: email`，证书**永远无法自动续签**（到期后 HTTPS 中断）。
  这是本轮顺带发现并修复的、比上面两条更直接的生产事故点。
- 新增（05:32:39Z）：`docker-compose.yml` 增加一次性迁移服务 `migrate`
  （`dashboard --migrate`，`restart: "no"`，等待 postgres `service_healthy`）；
  `gateway` 与 `dashboard-backend` 改为 `depends_on: migrate: service_completed_successfully`。
  原因：服务进程默认 `Serve` 模式（只校验、不建表），而编排里既没有 `DB_AUTO_MIGRATE`
  也没有迁移 Job —— **全新库直接 `docker compose up` 会因缺表启动失败**，已有库升级也不会创建新列。
- 修复（05:45:09Z）：`verify_database_schema()` 不再只检查**表**是否存在，同时校验关键**列**
  （`users.role`、`certificates.signing_started_at`、`certificates.expires_at`、
  `access_request_logs.requested_at` / `remote_addr`）。原先旧库升级后服务能"启动成功"，
  直到请求命中才报 `column does not exist`，极难定位。验证：临时改名 `users.role` 后能被正确检出。
- 变更（05:45:18Z）：`CONVERSATION.md` **合并进 [ISSUES.md](ISSUES.md) 附录 A 并删除原文件**，
  避免两份文档漂移；文中所有引用（`agent.md`、`TODO.md`、`CHANGELOG.md`、`crates/shared/src/database.rs`）
  已同步更新。
- 验证（本轮）：`cargo check --workspace --all-targets` 通过；前端 `vue-tsc -b && vite build` 通过；
  `gateway --migrate` 与 `dashboard --migrate` 打真实数据库幂等通过；`Serve` 模式启动正常。
  端到端用真实网关 + 自建上游（应答后关连接 / 保持连接两种）验证，见上。
- **未做 / 已知限制（诚实记录）**：上游连接**空闲复用率下降**。修复挂起后，连接任务一结束
  该连接就关闭，因此"客户端请求结束后的空闲复用"不再发生（同一客户端 keep-alive 连接内的
  连续请求仍会复用）。要恢复复用需要让上游连接的生命周期**脱离**客户端连接任务，属重构，
  本轮未做。功能正确性优先于复用率。

### 2026-10-02（第三轮：P0 止血 + 安全 + 前端放大器修复）

本轮按 [ISSUES.md](ISSUES.md)「建议的修复顺序」实施，范围 = **P0 止血 + 安全问题 + 前端放大器 + 角色权限模型**；
**不含**迁移方案（现为 [ISSUES.md](ISSUES.md) **附录 A**）中的分区/冷迁移部分（需停机迁移，另行排期）。

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
- 已知残留（本轮未做，明确记录以免误解）：① 上游连接池的空闲复用率仍然低（见 P0-6 说明）；② 访问日志表无分区/无 TTL，QPS 查询已走索引但表仍会无限增长（属 [ISSUES.md](ISSUES.md) 附录 A 迁移方案范围）；③ 网关仍是单点，未做多后端负载均衡（只用 `backends.first()`）与 PROXY protocol 解析；④ 前端 `eslint` 仍有 102 个**存量**错误（改动文件零新增），且 `eslint.config.ts` 依赖未声明的 `jiti`、未忽略 `dist`，建议后续单独修。

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
- AI 修改：归纳并交叉核对外部 AI 产出的《访问日志存储优化 + v1→v2 迁移方案》[CONVERSATION.md]，核对结论追加为 [ISSUES.md](ISSUES.md) 的「附：交叉核对」章节，并在 [agent.md](agent.md) 登记该文档。
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
