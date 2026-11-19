# Context Summary — 2026-11-19T04-31-44Z

## 当前状态
- 分支 `feat/database`（基于 `ci/test-workflow`）；主控实现已提交并推送（CI run 44359669547 全绿）。
- 本轮：时区规则 UTC+0 落地 + 网关缓存/IP-UA 技术栈记录；改动待提交/推送。

## 本轮变更
- beta 库：`ALTER DATABASE webgateway_beta_v2 SET timezone='UTC'`（新会话已确认 UTC）。
- `postgresql-dockerfile`：CMD 改为 `postgres -c timezone=UTC`。
- 集成测试：断言主控池与网关池 `SHOW TimeZone = UTC`。
- AGENT.md：新增 TimeZone 规则、网关缓存、IP/UA 拦截（低优先级）、gateways 上下线标记（中等优先级）。
- database.md：时区约定/环境表/websites.config 说明/决策记录更新。
- changelog.md 记录本轮。

## 验证
- `cargo fmt --check`、clippy `-D warnings`、单元测试（9）、集成测试（2，含 UTC 断言）全部通过。
- CI 待推送后确认（Rust job 已启用）。

## 关键事实
- sqlx 连接会话固定 `TimeZone=UTC`；数据库级默认也 UTC；周分区/统计日期按 UTC。
- beta 库 `webgateway_beta_v2` 已是 schema version 1（16 张表 + 4 周分区 ×4 + 扩展）。
- 面板按用户时区展示（前端后续）。
- 本地工具链：`RUSTUP_HOME=$PWD/.tools/rustup`、`CARGO_HOME=$PWD/.tools/cargo`；代理 `http://192.168.2.253:7890`。

## 下一步
1. 推送 `feat/database`，确认 CI。
2. 网关：`wait_for_master_schema` + `gateways` 上下线/心跳 + 缓存（主控配置：大小/类型/ETag/TTL）+ IP/UA 拦截 + 代理链路。
3. 面板时区展示、访问日志写入与秒/分/日归并、CRUD/认证/ACME。
