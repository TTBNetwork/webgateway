# Context Summary — 2026-11-19T17-05-53Z

## 当前状态
- 分支栈：`test/realtime-protocols` → `feat/stats-maintenance` → `fix/gateway-websocket-tls`，均已推送 GitHub。
- beta 库 schema **v3**；本轮联调/统计验证的临时数据已清理（websites / gateways / users / logs / stats = 0，
  `stats_state.last_aggregated_at = NULL`）；当前周 + 未来 3 周分区存在。
- 工作区干净；后台测试进程（Nezha / MCSManager / gateway / dashboard）已全部停止。

## 本轮完成

### 1. 实时协议联调（分支 `test/realtime-protocols`）
- Nezha Dashboard v2.6.2 + nezha-agent v2.4.0：gRPC h2c（18080）与 TLS/ALPN h2（18443）均注册成功；
  `ReportSystemInfo2` / `ReportSystemState` / `ReportGeoIP` 200，`RequestTask` 双向流保持打开，
  Dashboard `online_sessions.ongoing = 1` 约 37s；trailers 由 Go gRPC 客户端隐式验证。
- MCSManager v10.22.1（daemon 4.19.3）：Socket.IO 升级 101、daemon key 鉴权、`info/overview`、
  45s 内 2 次 engine.io ping/pong 通过。
- 新增 `scripts/e2e/mcsmanager_ws_probe.mjs`（零依赖）、`scripts/e2e/nezha_grpc_probe.sh`、
  `e2e-realtime.md`（拓扑/复现/结果）。
- 发现并记录 B-03（wss 上游不可用）与 B-19（升级连接无字节统计）。

### 2. P0 统计维护（分支 `feat/stats-maintenance`）
- 迁移 `crates/shared/migrations/0003_stats_maintenance.sql`：`stats_state` 单行水位、
  `wg_drop_expired_weekly_partitions()`、`wg_ensure_upcoming_weeks(3)`、schema v3。
- 统计任务移到 `crates/shared/src/stats.rs`：水位 + 至少重算最近 90s；缺口 > 1h 直接按分钟回填，
  最近 1h 写秒表；水位与聚合同事务推进；首个 tick + 每小时续建/回收分区。
- 集成测试 `crates/shared/tests/stats_rollup.rs`；beta 库实测：水位回拨 3h → 2h 前日志回填分钟级
  1/1、11/22 字节；重启后 8 周前分区 DROP；重复执行不重复计数。

### 3. WebSocket 上游 TLS + base path（分支 `fix/gateway-websocket-tls`）
- `state.rs` 抽出 `upstream_tls_config()`；`AppState.websocket_tls`（ALPN http/1.1）。
- `upgrade.rs` 对 https 后端先 TLS 握手再 HTTP/1.1 upgrade；上游 target = backend base path +
  客户端 path/query（`backend_request_target`）。
- E2E：明文 ws、wss（测试 CA）、wss + `/mc` base path 三种场景 PASS；网关单测 34。
- 测试证书必须由 CA 签发且 leaf 为 `CA:FALSE`，否则 rustls 报 `CaUsedAsEndEntity`。

## 测试
- 网关 34（+1 base path）；dashboard 2；shared 11 + 3 集成（database_init、stats_rollup、URL 替换）；
  `cargo fmt` / `clippy -D warnings` 全通过。
- 前端本轮未改。

## 关键事实
- 统计逻辑不再在 `dashboard/backend`，统一在 `crates/shared/src/stats.rs`；`dashboard` 只调用 `shared::stats::spawn`。
- `stats_state.last_aggregated_at` 为 NULL 时按 `now()-90s` 起步；跑满一轮后推进到 `now()-2s`。
- 分区名 `<parent>_<IYYY>_w<IW>`；整周 DROP 阈值 7 天；`wg_drop_partitions_older_than`（>=90 天）保留但未使用。
- E2E 用到的 Nezha/MCS 二进制与证书都在 `.tools/tmp/`（未跟踪），MCS 当前配置为非 SSL、无前缀。
- beta 库连接串不写入仓库；本机 Rust 工具链需设置
  `CARGO_HOME=.tools/cargo RUSTUP_HOME=.tools/rustup` 并优先使用 `.tools/rustup/.../bin`。

## 下一步（按 issue.md 优先级）
1. 面板 CRUD + 用户管理/RBAC/JWT 撤销（U-01/U-02）——本轮未开始。
2. B-04 / B-05 / B-06：h3 限制、优雅停机、日志可靠性。
3. U-11/U-13：h3 流式/Alt-Svc、h2 extended CONNECT。
4. U-17/U-19/U-23；B-21 分支合并。
