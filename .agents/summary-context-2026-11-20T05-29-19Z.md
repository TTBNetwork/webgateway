# Context Summary — 2026-11-20T05:29:19Z

## 当前状态
- 分支 `feat/observability`（叠加在 `feat/production-deploy` ee034e1 之后），HEAD a54eb85。
- 已 push 到 origin，CI 跟踪 a54eb85。
- schema 仍为 **v6**（本轮无迁移）；E2E 数据已清理（users/websites/certificates/gateways/logs/stats = 0），
  测试进程与端口已释放。

## 本轮完成（可观测性 B-25 / F-56）

### gateway 指标
- 新增 `gateway/src/metrics.rs`：独立 plain-HTTP `/metrics` 监听（hyper http1），
  配置 `[server].metrics_port` / `GATEWAY_METRICS_PORT`（默认关闭），
  `metrics_bind` / `GATEWAY_METRICS_BIND` 默认 `127.0.0.1`。
- 指标：`webgateway_requests_total`、`http3_requests_total`、`websocket_requests_total`、
  `connect_requests_total`、cache hit/miss/expired/store/eviction/rejected/clears、
  `cache_entries`/`cache_bytes`/`cache_max_bytes`、`connections_active`、`requests_in_flight`、
  `access_log_dropped_total`/`failed_batches_total`/`queue_available`、
  `config_sync_success_total`/`failures_total`、`route_hosts`、`certificate_hosts`、
  `webgateway_info{machine_id,node,version}`。
- 埋点：cache.rs（命中/过期/淘汰/拒绝/清空/占用）、listeners.rs（连接 gauge）、
  proxy.rs（请求 + 在途 guard）、http3.rs（h3 请求 + 在途）、upgrade.rs（WS/CONNECT 计数）、
  main.rs（同步成功/失败、metrics 任务生命周期）。
- 主循环初始同步失败也会计数；停机 abort metrics 任务。

### dashboard 指标
- 新增 `dashboard/backend/src/metrics.rs`：独立 axum 监听
  `DASHBOARD_METRICS_PORT` / `DASHBOARD_METRICS_BIND`（默认回环）。
- 指标：`dashboard_info{schema_version}`、`dashboard_up`、`dashboard_database_up`、
  uptime、站点/证书/网关（在线/总数）/用户计数、30 天内到期证书数、
  `dashboard_stats_ready`/`stats_lag_seconds`（读 stats_state）、
  `dashboard_auth_failures_total`/`auth_throttled_total`/`api_errors_total`。
- 埋点：router.rs 的登录失败/限流、internal() 错误；main 启动标记 + 任务生命周期。

### 文档
- `deployment.md`：dashboard/gateway 环境变量、限制表更新（告警需自建）、第 11 节
  抓取配置 + 完整指标清单 + 告警规则示例（DashboardDown/GatewayDown/AccessLogDropping/
  ConfigSyncFailing/CacheHitRatioLow/StatsLagging/CertificateExpiring/LoginFailures）。
- `gateway.md` 2.2 指标说明；两个 `config.example.toml` 增加 metrics 配置示例。
- `issue.md` B-25（部分修复）/P-12/U-19/F-56；`changelog.md` 新增小节。

## 实测
- dashboard `/metrics` 绑定 127.0.0.1:19100：`dashboard_info{schema_version="6"}`、
  `database_up 1`、`stats_lag_seconds` 正常；错误 TOTP 后 `auth_failures_total 1`。
- gateway `/metrics` 绑定 127.0.0.1:19101；同一站点请求两次：
  `requests_total 2`、`cache_misses_total 1`、`cache_hits_total 1`、`cache_stores_total 1`、
  `cache_entries 1`、`route_hosts 1`、`config_sync_success_total 1`；访问日志队列 4096。
- `ss` 确认指标端口只在 127.0.0.1 监听，站点端口不受影响。

## 测试
- gateway 39（+2 metrics 渲染/转义）、dashboard 25（+2）、shared 18 + 3 集成；
  `cargo fmt` / `clippy -D warnings` 通过。
- 注意：CI 使用 `--locked`；本轮未新增依赖（gateway 复用 hyper/hyper-util，dashboard 用 axum）。

## 关键事实 / gotchas
- 指标端点无认证，默认只绑定回环；跨机抓取需隧道/内网，不要暴露公网。
- 计数器为进程内 AtomicU64，重启归零；告警用 rate/increase。
- gateway 指标端口与站点端口独立；未配置时不监听。
- 缓存每次配置同步会 clear（`cache_clears_total` 会 +1），属预期。
- 访问日志队列可用量来自 mpsc capacity（4096）。

## 下一步（生产清单剩余）
1. 上游健康检查/失败摘除（P-10/U-17）与按 IP 限流（U-07）。
2. HTTP/3 流式响应、缓存接入、停机 drain（B-04/P-01）。
3. 缓存正确性：Vary、single-flight、HEAD/304（B-09）。
4. 统计：total_ips、保留期配置、UTC 日（B-08/B-14）。
5. refresh token / 每用户 API Key / 服务令牌过期（B-30 剩余）。
6. B-21 分支整合（需用户确认）；CI DB E2E（U-31）依赖 PG18+uint128 镜像。
