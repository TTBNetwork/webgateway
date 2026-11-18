# Context Summary — 2026-11-18T16-56-25Z

## 当前状态
- 仓库：`/develop/rust/webgateway_v2`；分支 `ci/test-workflow`（已推送，CI 通过）。
- 工作区有未提交文件：`database.md`（设计稿，待用户确认）、`changelog.md`、`.agents/`。
- 当前任务：数据库实现（主控初始化 + 网关等待），停在**设计稿待确认**，尚未生成 Rust 代码。

## 已完成
- CI 测试流程：workflow + 两个复合 action + pnpm 锁文件 + 前端 CI 脚本（commit 已推送）。
- 数据库设计稿 `database.md`：完整表结构、启动时序、迁移方案、测试方案。

## 关键决策（已定稿，待整体确认）
- 主控独占迁移：sqlx 迁移文件编译进 dashboard 二进制；网关没有迁移代码路径，只轮询 `schema_state`。
- 就绪标记：`schema_state` 单行表；每个迁移文件末尾 `SELECT wg_mark_schema_version(N)`，与 DDL 同事务原子提交。
- 网关等待：默认 300s（`DATABASE_WAIT_TIMEOUT=0` 无限）；表不存在（42P01）/连接失败继续等，其它 SQL 错误立即失败；版本高于期望 WARN 继续。
- 扩展：使用内部镜像的 `uint128`（类型族 `uint1/2/4/8/16`，按字节命名）与 `btree_gin`；
  `ports uint2[]`、`body_length uint8`、`status uint2`、`GIN(name)` 依赖 btree_gin。`CREATE EXTENSION` 可放在迁移事务内。
- 表：`schema_state` / `configurations` / `users` / `users_client_secrets` / `users_info` / `web_log` /
  `dns_providers` / `certificates` / `websites` + 四张周分区访问日志 + `access_stats_daily`。
- 配置类表（configurations/websites/certificates/dns_providers）挂 `*_updater` NOTIFY；`updated_at` 用 `wg_update_updated_at()` 触发器维护。

## 环境事实
- beta PG：PostgreSQL 18.2；库 `webgateway_beta_v2` 当前为空（仅 `plpgsql`）；角色 `webgateway` 为库 owner + superuser；
  UTF8 + C collation；`Asia/Shanghai`。可用扩展：`uint128 1.1.1`、`btree_gin 1.3`、`pg_cron 1.6`、`vchord 0.0.0`。
- 连接串不写入仓库，只通过 `DATABASE_URL` 注入（用户已在对话中提供，勿提交）。
- 本机库客户端工具：`.tools/pgsql/lib/libpq.so.5` + `.tools/pq.py`（已加入 `.git/info/exclude`，不进 git）。
- 网络不通时可用代理 `http://192.168.2.253:7890`。

## 下一步
1. 等用户确认 `database.md`（剩余问题见文档第 11 节：迁移方式、表范围、就绪语义、超时策略、beta 库初始化授权）。
2. 新建功能分支 `feat/database`；生成 workspace（`crates/shared` / `dashboard/backend` / `gateway`）、迁移 SQL、
   `shared::database`（`initialize_master` / `wait_for_master_schema` / `verify_schema`）、最小二进制。
3. 单元测试 + 集成测试（临时库），在 beta 库执行一次初始化，观察网关「等待 -> 就绪」。
4. 提交并推送触发 CI（Rust job 会自动启用），更新 `changelog.md` 和本目录上下文总结。

## 注意
- AGENT.md 要求：完成后写 `changelog.md` 与 `.agents/summary-context-{datetime}.md`。
- 不提交凭据 / 真实连接串；`config.toml` 需 gitignore，只提交 `config.example.toml`。
