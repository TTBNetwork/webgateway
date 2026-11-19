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
