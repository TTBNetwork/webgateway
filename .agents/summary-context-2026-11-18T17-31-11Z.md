# Context Summary — 2026-11-18T17-31-11Z

## 当前状态
- 仓库：`/develop/rust/webgateway_v2`；数据库主控实现完成并已在 beta 库验证，等待提交/推送。
- 分支规划：从 `ci/test-workflow` 新建 `feat/database`（CI 基础设施已在 `ci/test-workflow`）。
- 未提交文件：`Cargo.toml` / `Cargo.lock` / `crates/` / `dashboard/backend/` / `database.md` /
  `changelog.md` / `.agents/` / `postgresql-dockerfile` / `AGENT.md` / `.gitignore`。

## 已完成
- Rust workspace：`crates/shared`、`dashboard/backend`（edition 2024、resolver 3、依赖统一在 workspace）。
- shared：`DatabaseConfig`、`Database`、`initialize_master`、`verify_schema`、`wait_for_master_schema`、
  `health_check`、`schema_version()`、URL 脱敏；tracing 初始化；错误/日志英文。
- 迁移 `crates/shared/migrations/0001_initial_schema.sql`：扩展、schema_state、配置/用户/证书/DNS/站点、
  gateways + `gateways_online` 视图、四张周分区日志、`access_stats_second/minute/daily`、NOTIFY/updated_at 触发器。
- dashboard 主控：迁移 + `GET /health`，配置 `config.example.toml`。
- 测试：单元 9 个；集成测试创建临时库，覆盖未初始化等待超时、主控初始化、幂等、网关就绪。

## 验证结果
- `cargo fmt --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace` 全部通过。
- 集成测试在 beta 服务器创建临时库并成功迁移/清理。
- beta 库 `webgateway_beta_v2` 已实际初始化：schema version 1、16 张表（含 4 个周分区 × 4）、
  `uint128`/`btree_gin` 已安装、触发器/视图齐全；主控 `/health` 返回 200。
- 注意：sqlx 连接设置 `TimeZone=UTC`，周分区/统计日期按 UTC；分区边界实测 `2026-11-16 08:00+08`。

## 关键决策
- 不迁移 v1 数据；先做主控，网关下一步。
- status 不加 RFC CHECK；扩展使用内部镜像的 uint128/btree_gin。
- 网关上下线写 `gateways` 表 + 心跳（中等优先级，下一步实现）。
- 统计粒度：1 小时内秒级，超过 1 小时归并分钟级，日级汇总（归并任务下一步）。
- sqlx 迁移文件、主控独占执行；网关无 DDL 路径。

## 本地开发环境（沙箱内）
- `/root` 只读，所以工具链/依赖放在 `.tools/`（已加入 `.git/info/exclude`）：
  `RUSTUP_HOME=$PWD/.tools/rustup`、`CARGO_HOME=$PWD/.tools/cargo`；stable 1.100.0 已安装。
- 代理：`http://192.168.2.253:7890`（cargo/rustup/curl 可用）。
- 数据库客户端：`.tools/pgsql/lib/libpq.so.5` + `.tools/pq.py`（libpq ctypes 查询工具）。
- beta 连接串只在命令行/环境变量中使用，不写入仓库。

## 下一步
1. 提交：docs（database.md、postgresql-dockerfile、AGENT.md）、代码（workspace/shared/dashboard）、
   changelog/.agents；推送 `feat/database`，等待 GitHub Actions（Rust job 会自动启用）。
2. 网关：`wait_for_master_schema` + `gateways` 上下线/心跳 + 代理链路。
3. 访问日志写入与秒/分/日归并、配置 NOTIFY 同步、CRUD/API、认证、ACME、前端。
