# Context Summary — 2026-11-19T23-15-40Z

## 当前状态
- 分支 `feat/reliability-hardening`（叠加在 `feat/dashboard-rbac` 之后）。
- beta 库 schema **v6**；可靠性/认证测试数据已清理（users/websites/certificates/gateways/logs/stats = 0），
  `configurations` 测试 ACME key 已删除，`stats_state.last_aggregated_at = NULL`。
- 测试进程已停止；`.secrets/` 仍被 gitignore。

## 本轮完成

### 迁移
- `0006_totp_replay.sql`：`users.last_totp_step BIGINT`；schema v6；shared 校验/测试同步。

### B-29 TOTP 防重放 + 登录限速
- `auth.rs`：`LoginOutcome { Success, Invalid, Throttled }`；
  - TOTP 校验返回匹配 step；`UPDATE ... WHERE last_totp_step IS NULL OR last_totp_step < $step`
    原子防重放；重放按失败计数；
  - 进程内 `LoginLimiter`（user+IP，5 次失败/5 分钟锁 5 分钟）；登录返回 429 + retry_after_secs。
- 实测：同一 code 第二次 401；连续失败后正确 code 也 429；retry_after_secs=299。
- 限制：限速计数单实例内存，多主控不共享。

### B-06 访问日志可靠性
- `access.rs`：写库失败重试 3 次（200/400ms 退避）；失败记录 dropped/failed 计数并 WARN；
  channel full 每 100 条 WARN；新增 `AccessWriter::shutdown()`，watch 信号触发 drain + flush。
- `proxy.rs` log_event 使用 `record_dropped()`。
- 单测：dropped 计数递增。

### B-05 优雅停机
- `listeners.rs`：`Inflight` 计数 + watch shutdown；SIGTERM 后停止 accept，
  最多等待 10s 让在途 HTTP/TCP 请求完成；`Listeners::shutdown()` 返回是否 drain 完成。
- `main.rs`：停止 accept → drain → abort h3 accept → flush access log → abort heartbeat →
  mark offline → 关闭连接池。
- 实测：慢请求（3s）在 SIGTERM 后仍 200（body slow-ok），exit 0，access log 写入，
  gateway status offline；日志 `access log writer stopped count=1`。
- 限制：HTTP/3 在途请求仍未 drain（仅 abort accept loop）。

## 测试
- gateway 36、dashboard 10、shared 14 + 3 集成（2 database_init + 1 stats）；
  `cargo fmt` / `cargo clippy --workspace --all-targets -D warnings` 通过。
- 本轮未改前端。

## 关键事实
- 优雅停机只对 TCP/HTTP listener 做 in-flight 跟踪；WebSocket 长连接会等满 10s 后被关闭。
- AccessWriter 的 shutdown 必须在连接池关闭之前调用；main 已按此顺序。
- 登录限速 key：`user:<lowercase>` + `ip:<x-forwarded-for 第一段 / x-real-ip>`；成功登录会清除两个 key。
- 迁移 0006 后 admin 的 last_totp_step 初始为 NULL，首次登录正常写入。
- 测试用慢后端在 `.tools/tmp/slow_server.py`，未跟踪。

## 下一步（issue.md）
1. B-04：HTTP/3 body/response 上限、缓存接入；B-11 全局/站点 body/头/连接限制。
2. 日志落盘 spill + /metrics；登录限速多实例共享（DB/Redis）。
3. JWT refresh / 每用户 API key / 服务令牌轮换；TOTP 限速告警。
4. B-07 CONNECT 日志/端口白名单、B-12 Alt-Svc、U-11/U-13、分支合并。
