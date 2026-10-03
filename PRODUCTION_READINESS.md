# 生产上线评估报告

> 初版 2026-10-02 05:45:18Z，**更新于 2026-10-02 07:58:41Z**。
> **注意：2026-10-02 约 07:07Z 发生生产事故（上线后两个服务全部起不来），
> 原因是第七轮的自动迁移把重活放进了启动路径 —— 详见第九节与 [CHANGELOG.md](CHANGELOG.md) 第八轮。**
> 对应 [STEP.md](STEP.md) 的要求：说明改了什么、能不能在生产环境运行、不能则说明原因。
> 详细逐条变更见 [CHANGELOG.md](CHANGELOG.md) 第四/五轮；问题清单见 [ISSUES.md](ISSUES.md)；
> 会话上下文见 [agent-context.md](agent-context.md)。

## 一、结论

**代码侧：可以上生产。** 修掉了 6 个必然导致生产事故的缺陷（含 1 个上一轮引入的回归、
1 个必现的证书续签失效、1 处 **7 倍存储膨胀**），打通了迁移链路，解除了 release 构建阻塞，
并补上了数据库保留期清理。所有改动都在真实 PostgreSQL 上做过端到端验证，
且在生产日志里找到了 `operation was canceled` 的实证（8099 次）。

**部署侧：release 产物已可构建；剩下的是「执行上线」这一步，需要你确认。**

| # | 事项 | 状态 |
|---|------|------|
| 1 | 代码缺陷 | ✅ 已修复并验证 |
| 2 | 在线迁移链路 | ✅ 已打通（compose 增加一次性 `migrate` 服务） |
| 3 | 生产库连通性 | ✅ 已连通（库名是 `postgres`，不是 `webgateway`）；**生产库缺 `users.role` 与 `certificates.signing_started_at`，上线前必须迁移** |
| 4 | release 构建 | ✅ 已解阻 —— vendor `acmex` 去掉硬编码 fips（见第五节）；`cargo build --release` 退出码 0（2026-10-02 05:58Z） |
| 5 | 生产上线执行 | ⏳ **未执行** —— 迁移与镜像更新都需要你确认后操作 |
| 6 | 数据库容量护栏 | ✅ 已实现保留期清理（面板可配 90~3650 天，默认 180）；size 表改为**按秒聚合**，消除 7 倍膨胀（见第六节） |
| 7 | 无感迁移 | ⚠️ **第七轮实现有缺陷并导致生产事故**（见第九节）；第八轮已修复为"与表大小无关"的廉价迁移 |

## 二、改了什么

| 文件 | 改动 | 验证方式 |
|------|------|---------|
| `gateway/src/upstream.rs` | ① 修复"连接任务结束后连接仍被放回空闲池"导致后续请求**永久挂起**；② 改用 `try_send_request` 实现"复用连接已被上游关闭"时安全重试一次 | 真实网关 + 自建上游（应答后关连接 / 保持连接）各 6/6 通过；`curl` 50/50 通过 |
| `gateway/src/upstream/connection.rs` | 新增 `task_exited` 标志与 `is_reused()`；`is_reusable()` 改为「响应体读到 EOF **且** 驱动任务仍在运行」 | 同上 |
| `crates/shared/src/database/access.rs` | 批量 INSERT 改为幂等（`ON CONFLICT DO NOTHING`），消除"部分成功后整批重试永久失败" | 真实库：整批重试 / 部分成功后整批重试，均通过 |
| `crates/shared/src/database/certificate.rs` | ① 认领增加 10 分钟过期回收；② **修复 SELECT 漏选 `email` 列导致自动续签 100% 失败** | 真实库三场景：不抢占 / 回收僵死认领 / 不重复签发 |
| `crates/shared/src/database.rs` | `verify_database_schema()` 增加**列**校验（原先只查表） | 临时改名 `users.role` 能被正确检出 |
| `docker-compose.yml` | 新增一次性 `migrate` 服务；两个服务改为等它成功后再启动 | 迁移对真实库幂等通过 |
| `vendor/acmex/` + `Cargo.toml` | vendor acmex 去掉硬编码 fips；`[patch.crates-io]` 重定向 | `cargo build --release` 退出 0；`cargo tree` 中 fips-sys 计数 0 |
| `dashboard/frontend/*` + CI | yarn → pnpm；锁文件/配置/CI 全部迁移 | `pnpm install --frozen-lockfile` + `pnpm build` 均通过 |
| 文档 | `CONVERSATION.md` 合并进 `ISSUES.md` 附录 A 并删除；`STEP.md` 融合进 `agent.md`；新增 `agent-context.md` | 全仓无失效引用 |

### 最值得注意的一条

`crates/shared/src/database/certificate.rs` 的认领查询**漏选了 `email` 列**，而 `NeedSignCertificate`
的 `FromRow` 要求该列存在。这意味着：**只要库里有满足续签条件的证书，认领查询就会报
`no column found for name: email`，自动续签永远不可能成功**，证书到期后 HTTPS 直接中断。
这不是"概率问题"，是必现功能失效。

## 三、怎么上线

```bash
cp .env.default .env        # 设置 SUBNET_PREFIX、POSTGRES_PASSWORD
docker compose up -d        # 服务启动时自动迁移（AutoMigrate），无需额外步骤
docker compose logs -f gateway dashboard-backend   # 观察迁移日志
```

- 两个服务默认 `DB_AUTO_MIGRATE=1`（即使不设置，程序默认也是 `AutoMigrate`）：
  启动时在 `pg_advisory_xact_lock(SCHEMA_INIT)` 下补齐**完整** schema 再进入服务。
  **gateway 与 dashboard 谁先启动都一样** —— 控制面表（`users` / `users_client_secrets` /
  `web_log`）也已纳入共享迁移入口。迁移幂等，重复 `up` 安全。
- **全新库**：直接 `up` 即可，不会再出现"缺表启动失败"。
- **已有库升级**：同样自动补齐新列（`users.role`、`certificates.signing_started_at`）、新表
  （`configurations`）与 size 表的 `at_second` + 唯一索引。**首次会明显变慢**（要合并约 417 万行
  历史重复数据，见第六节），建议低峰期部署并确认磁盘余量。
- 只想服务、不执行 DDL：设 `DB_AUTO_MIGRATE=0`（逃生开关）。此时 `Serve` 模式会校验表**与关键列**，
  缺失时直接报错，不会再出现"启动成功、请求时才报 column does not exist"。

## 四、生产环境核查结果（2026-10-02 06:00Z，SSH 只读排查）

生产库已连通且只读巡检完毕。**关键：库名是 `postgres`，不是 `webgateway`。**

| 项 | 现状 |
|----|------|
| 主机 | `sj-pub`，Debian 13，2 vCPU / **1.9 GiB 内存（无 swap）**，根分区 39G 已用 50% |
| PostgreSQL | 18.2，扩展 `btree_gin` / `uint128` / `plpgsql` 齐全 |
| 部署目录 | `/opt/webgateway/`（compose + `.env` + `data/`） |
| 镜像来源 | **`ghcr.milu.moe`**（镜像站），不是 `ghcr.io` |
| 运行版本 | 镜像 3 周前构建，**早于全部审计修复**（日志无新代码的 `Database startup mode: Serve`） |
| 迁移方式 | 旧镜像靠启动时建表；第七轮起新镜像默认自迁移（`AutoMigrate`），无需额外 Job |

### 上线要做的事（第七轮后已简化）

1. **直接部署新镜像即可**：两个服务默认自迁移，会自动补齐生产库缺失的
   `users.role`、`certificates.signing_started_at`、`configurations` 表，以及 size 表的
   `at_second` + 唯一索引。
2. **选低峰期、确认磁盘余量**：首次启动要合并约 417 万行历史重复数据（见第六节），
   是一次性代价；迁移为单事务，失败整体回滚、重跑安全。
3. 确认 `/opt/webgateway/.env` **没有** `DB_AUTO_MIGRATE=0`（那是"只服务不迁移"的逃生开关）。

### 生产日志实证了本轮修复的必要性

| 日志内容 | 出现次数 |
|---------|---------|
| `Failed to send request: operation was canceled` | **8099** |
| `hyper::Error(Canceled, IncompleteMessage)` | 7897 |
| `BadCertificate` / `tls handshake eof` / `NoCipherSuitesInCommon` | 25.7 万 / 10.4 万 / 6.6 万 |

也就是说，你报告的 `operation was canceled` 在生产是**高频真实故障**，不是偶发。
另外旧镜像仍在跑 `qps_per_5s` 视图，dashboard 日志显示单次查询 **4.66 秒**、前端每 5 秒轮询一次。

### 数据量（迁移与容量的直接约束）

| 表 | 行数 | 大小 |
|----|------|------|
| `access_response_size_logs` | ~15,565,769 | **3606 MB** |
| `access_request_logs` | ~2,300,593 | 1697 MB |
| `access_response_logs` | ~2,090,720 | 972 MB |
| `access_request_size_logs` | ~57,515 | 15 MB |

合计约 6.3 GB，主机磁盘剩余 19 GB。**日志表无分区/TTL**，这部分是当前最大的容量风险。

### 另外两点值得注意

- 生产两张通配符证书（`*.ttb-network.top` / `*.atxa.top` / `*.txit.top`）的 `email` 与
  `dns_provider_id` **都是 NULL**，因此不参与自动续签；其中一张已于 2026-09-03 过期。
  要自动续签需在面板补这两个字段。
- Postgres 的 5432 直接暴露在公网（`docker-proxy` 监听 `0.0.0.0:5432`），建议改为仅内网可达。

## 五、release 构建：已解阻（2026-10-02 05:58Z）

```
$ cargo build --release --bin dashboard --bin gateway --bin webgateway-mnt
error while processing "\t.section\t.data.rel.ro.local,\"aw\"\n" on line 226412: ".data section found in module"
gmake[2]: *** [.../bcm-delocated.S] Error 1
=> 退出码 101
```

根因：`acmex 0.8.0` 的 `Cargo.toml` 里 `aws-lc-rs = { features = ["fips"] }` 是**硬编码且非可选**的，
于是必然编译 FIPS 验证过的 C/汇编后端 `aws-lc-fips-sys`。本机 GCC 15.2 + binutils 2.46 会拒绝
其 `.data.rel.ro.local` 段，属上游已知问题（[aws-lc-rs#614](https://github.com/aws/aws-lc-rs/issues/614)、
[#615](https://github.com/aws/aws-lc-rs/issues/615)）。

**根因确认过程**：升级 `aws-lc-fips-sys` 0.13.14 → 0.13.17 **无效**（同样报错）；
而单独编译非 FIPS 的 `aws-lc-sys` 在本机 **38 秒通过**。进一步确认 acmex 全仓**只有一处**
用到 aws-lc-rs（`src/storage/encrypted.rs` 的 AES-256-GCM），与 FIPS 认证无关。

**采用的修法**：`vendor/acmex/`（569 KB，只保留 `src/`、`Cargo.toml` 与 LICENSE）并去掉硬编码的
`features = ["fips"]`，根 `Cargo.toml` 用 `[patch.crates-io]` 重定向；`dashboard/backend/Cargo.toml`
仍写 crates.io 版本号，便于上游修复后一行回退。补丁处写明了原因。

**验证**：
- `cargo build --release --bin dashboard --bin gateway --bin webgateway-mnt` → **退出码 0**，三个二进制均产出；
- `cargo tree -p dashboard` 中 `aws-lc-fips-sys` 计数 **0**；
- 两个 release 二进制对真实库跑 `--migrate` 均退出 0。

**遗留**：CI 仍只构建三个 bin，不构建 `crates/acme`（那个 crate 的 `include_str!` 写死了本机绝对路径，
`--workspace` 在 CI 必挂）。当前 CI 因此不会暴露该问题，但也意味着它从未被验证过。

## 六、size 明细表：7 倍膨胀与按秒颗粒度（第六、七轮）

巡检生产库时发现 `access_response_size_logs` 有 **1600 万行 / 3.6 GB**，是请求主表（228 万行）的
**7.03 倍**；近 24 小时仍在以 **6.40 倍**的重复率增长；单条 `response_id` 最多挂了 **262791 行**。

根因：`StatisticsIncoming::poll_frame` 每读到一个 body chunk 就调一次
`insert_increase_response_size_log`，而该函数**为每次调用新生成一个 ObjectId 并插一行**。

**修法（第七轮定稿）**：这两张表的用途是"按秒统计请求/响应体大小"，因此按 **`(请求, 秒)`** 聚合：

| 项 | 说明 |
|----|------|
| 唯一键 | `(xxx_id, at_second)`，`at_second` 截断到整秒 |
| 写入 | 内存按 `(id, 秒)` 累加 → 单事务 upsert `body_length = body_length + EXCLUDED.body_length` |
| 效果 | 同一秒内合并成一行；**跨秒仍分开** —— 行数按"请求数 × 跨秒数"收敛，秒级颗粒度不丢 |

（第六轮曾把整个请求聚合成一行，虽然更省空间但**丢了秒级颗粒度**，第七轮已纠正。）

### 迁移现在是廉价的（第八轮修复后）

原方案（第七轮）要做「补列 → 回填 1600 万行 → 合并 417 万行 → 建全量唯一索引」，
全部在**启动路径**的单个事务里，导致生产停机（见第九节）。现在改为：

| 操作 | 代价 |
|------|------|
| `ADD COLUMN at_second TIMESTAMPTZ`（**可空**） | 元数据操作，**不重写表** |
| `CREATE UNIQUE INDEX ... WHERE at_second IS NOT NULL` | **部分索引**，老行被谓词排除，无需回填/去重 |
| 写入 | `ON CONFLICT (xxx_id, at_second) WHERE at_second IS NOT NULL DO UPDATE ...` |

实测迁移耗时 **356 ms**（修复前同一库上需要数分钟）。
历史行 `at_second` 保持 NULL（它们的秒级信息本来就在 `created_at` 上，逐个 chunk 一行），
**不参与**唯一索引、也不需要任何回填或去重。

**另需注意**：这不会让已有的 1600 万行瘦身 —— 瘦身要靠保留期清理（默认 180 天）。
`DELETE` 腾出的空间由 autovacuum 回收并留给同表复用，**不会立刻归还操作系统**。

按当前速率（约 12.8 万响应/天），若不做保留期清理，半年后这张表约 80 GB —— 保留期
（默认 180 天，见第六节）是配套的长期护栏。

## 七、无感迁移（第七轮）

两个服务默认 `AutoMigrate`：启动时在 `locks::SCHEMA_INIT` 事务级 advisory lock 下补齐**完整**
schema（含控制面的 `users` / `users_client_secrets` / `web_log`）再进入服务。
因此 **gateway 与 dashboard 谁先启动都一样**，全新库直接 `docker compose up -d` 即可，
不再需要一次性 migrate 服务（compose 已移除）。

控制面表的 DDL 已从 dashboard 挪到 `crates/shared/src/database/dashboard_schema.rs` ——
原先它们在 dashboard 的迁移里，gateway 调不到，所以"gateway 先启动"时会缺 `users` 表，
而 gateway 的 schema 校验恰恰要求它存在。

逃生开关：`DB_AUTO_MIGRATE=0`（或 `false`）＝只校验不建表（需要自行保证 schema 已就绪）。

## 八、本次未做（诚实记录）

- **上游连接空闲复用率下降**：修复挂起问题的代价是"连接任务结束即关闭连接"，
  因此客户端请求结束后的空闲复用不再发生（同一客户端 keep-alive 连接内的连续请求仍复用）。
  要恢复复用需让上游连接生命周期脱离客户端连接任务，属重构。
- ~~访问日志表仍**无分区**~~ **已完成**：第十轮做了按周 RANGE 分区结构，第十一轮做了
  v1→v2 的后台自动迁移（零停机），见 [CHANGELOG.md](CHANGELOG.md) 第十/十一轮与本文第十节。
- 网关仍是单点，未做多后端负载均衡与 PROXY protocol 解析。
- CI 仍无 `fmt` / `clippy` / `test` 门禁。
- 生产迁移与镜像更新**未执行**（需要你确认后操作）。
- `assets/error_pages/` 仍使用 yarn（它不参与 CI 构建，本轮未动）。
- 生产 TLS 握手失败量级较大（`BadCertificate` 25.7 万次），未做进一步归类分析。
- 控制面板的代理配置未做成字段（核实后确认用环境变量即可，且证书续签本轮明确不做）。
- size 明细表的**历史去重**由迁移自动完成，但生产库首次启动会因此变慢（约 417 万行）。
- ~~分区（按周 RANGE）方案仍未做~~ **已完成**：结构见第十轮，自动迁移见第十一轮（本文第十节）。
  当前折中是"按周分区 + 保留期整周 DROP + 按秒聚合"。

## 九、2026-10-02 生产事故复盘（上线后服务全部起不来）

| 项 | 内容 |
|----|------|
| 时间 | 约 07:07Z 部署 → 07:14Z 起两个服务全部不可用；07:26Z 定位并修复代码 |
| 现象 | gateway 无法代理、dashboard 登不进去；容器 `Up` 但服务未就绪 |
| 直接原因 | 两个进程都阻塞在 `SELECT pg_advisory_xact_lock(SCHEMA_INIT)`，日志显示已等 **279 秒** |
| 根因 | 第七轮的自动迁移把「回填 1600 万行 + 合并 417 万行 + 建全量唯一索引」放进**启动路径**的单事务，全程持锁数分钟 |
| 放大因素 | 容器被健康检查判定为"起不来"→ 重启 → 迁移整体回滚 → **重来一遍**，形成死循环 |
| 修复 | ① 迁移改成与表大小无关（可空列 + 部分唯一索引），实测 356 ms；② 取锁加 60 秒上限，超时快速失败并给出可操作提示；③ size 表累加写入改为 `DELETE + INSERT`（见下） |
| 恢复情况 | 事故中的迁移**最终跑完了**（旧版全量唯一索引已建成），两个服务已自行恢复：网关 HTTP/HTTPS 均返回 **200**、前端 200、访问日志持续写入 |
| 遗留故障 | 恢复后发现响应大小统计在报外键错误（父行缺失导致整批重试卡死），已在第八轮一并修复（父行守卫） |
| 责任 | 设计失误在我（第七轮）。已在 [agent.md](agent.md) 写入硬性规则：启动路径只允许与表大小无关的 DDL |

### 当前状态与后续动作

生产**已恢复**：网关返回 200、size 行持续写入，容器重启计数停在 3 次。
但有一个**必须在下次部署前解决**的兼容性问题：

| | 生产库现状（旧版迁移跑完） | 仓库新方案 |
|---|---|---|
| `at_second` | `NOT NULL` | 可空 |
| 唯一索引 | **全量** `(xxx_id, at_second)` | **部分** `WHERE at_second IS NOT NULL` |
| `ON CONFLICT` 写法 | 认不带谓词的版本 | 只认带谓词的版本 |

两种写法互不兼容 —— 若代码只认一种，**下一次部署就会让响应大小刷盘整批失败**。
第八轮已改为 `DELETE 该行 + INSERT 累加值`（不依赖冲突推断，对两种索引都成立，已实测），
因此**仓库当前代码可以直接部署到现在的生产库上**。

后续动作：

1. 部署含第八轮修复的镜像即可（迁移是廉价的，几百毫秒完成）。
2. 部署后观察日志应出现 `Database startup mode: AutoMigrate` 后**立即**
   `Dashboard API listening on port ...` / `Listening on ...`，不再有长时间的
   `pg_advisory_xact_lock` 慢查询。
3. 若仍有异常，**不要反复重启**（重启会让迁移回滚重来）；改为
   `docker compose run --rm <服务> --migrate` 单独跑一次迁移。
4. 部署后确认日志**不再出现** `Failed to flush access response size inserts`；
   并可用 `SELECT COUNT(*) FROM access_response_size_logs WHERE created_at > NOW() - INTERVAL '5 minutes'`
   确认 size 行持续写入。
4. 可选清理：生产库里同时存在"全量唯一索引"和仓库新建的"部分唯一索引"是不必要的重复，
   稳定后可手工 `DROP INDEX` 掉其中一个（建议保留部分索引，`at_second` 也就可以恢复可空）。

## 十、统计表 v2 分区化方案（STEP.md 目前-1 / 目前-4，**待确认后才动手**）

用户要求：统计表改 v2 并支持**按周轮换**，迁移后做查询优化以加速面板。
完整 DDL 与取舍见 [ISSUES.md](ISSUES.md) **附录 A**。下面是**在这套生产数据上**的执行评估。

### 现状约束（2026-10-02 只读实测）

| 表 | 行数 | 大小 | 备注 |
|----|------|------|------|
| `access_request_logs` | ~228 万 | 1697 MB | PK `id` |
| `access_response_logs` | ~222 万 | 972 MB | PK `id`，**FK → request_logs.id** |
| `access_response_size_logs` | ~1157 万 | 3606 MB | **FK → response_logs.id** |
| `access_request_size_logs` | 5.8 万 | 15 MB | **FK → request_logs.id** |

分区化的硬性代价（附录 A 已列，这里重复强调，因为它决定风险）：

1. **分区键必须进主键** → `PRIMARY KEY (id, requested_at)`；
2. **外键必须全部移除** —— PostgreSQL 的分区表无法被单列 `id` 引用，
   也就是要放弃 `response_logs → request_logs`、两张 size 表 → 主表的引用完整性；
3. 视图改为参数化函数（视图无法裁剪分区）；
4. **数据要搬** —— 不搬的话老数据留在父表、新数据进分区，查询反而更慢。

### 执行方案（建议）

**这不是一次"启动时迁移"，必须当成有计划的数据搬迁来做**（第八轮事故的教训）：

| 阶段 | 动作 | 影响 |
|------|------|------|
| 0 | 在 beta 库上用生产的行数量级演练一遍，记录耗时与峰值磁盘 | 无 |
| 1 | 建 v2 分区表（`access_*_v2`）+ 建本周/下周分区 | 无（不影响线上） |
| 2 | 分批把历史数据搬进 v2（`INSERT ... SELECT` 按周分段，每段一个事务；**分批提交**） | 只增磁盘 IO，不锁线上表 |
| 3 | 记录迁移期间新写入的水位，搬迁结束后补齐增量 | 同上 |
| 4 | 切读：改查询指向 v2（大小查询改走 `COALESCE(size_logs, 主表)`） | 短暂 |
| 5 | 切写：gateway 改双写到 v2 或直接切 v2；观察一个周期 | 短暂 |
| 6 | 校验行数/汇总一致后 `DROP` 旧表、`ALTER TABLE ... RENAME` | 短暂，之后回收空间 |

### 风险与前置条件（必须先确认）

- **磁盘**：v2 与旧表并存期间约为现有数据的 2 倍（约 12 GB），主机当时只剩约 19 GB —— **偏紧**，
  建议先做保留期清理（面板「设置 → 数据保留」可设 90 天）腾出空间，或外部备份后清空历史。
- **停机窗口**：阶段 4~6 需要短暂停写；建议在低峰期，并准备回滚脚本（回滚 = 把查询指回旧表）。
- **引用完整性**：移除外键后，孤儿行不会再被数据库拦下 —— 需要保留 `parent 守卫`
  （第八轮已加在累加写入里）作为应用层保障。
- **保留期清理要改**：分区化后清理应改为 `DETACH PARTITION` + `DROP`，
  比逐行 `DELETE` 快几个数量级，也不再需要 autovacuum 回收。
- **`schema_version` 表**：附录 A 提到但尚未实现；v2 迁移属于不可幂等的结构变更，
  建议**先补这张表**再做迁移。

### 建议的推进顺序

1. 先做**目前-4 的低风险部分**：面板慢查询的既有索引/查询改写（不改表结构，可立即收益）。
2. 空间腾出后，在 beta 上按生产量级演练附录 A 的搬迁脚本。
3. 再排停机窗口执行阶段 1~6。

**需要你确认**：是否按上面的方案推进？以及能否给出一个可用的低峰停机窗口？
