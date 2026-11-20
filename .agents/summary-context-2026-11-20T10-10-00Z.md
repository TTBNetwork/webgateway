# Context Summary — 2026-11-20T10:10:00Z

## 当前状态
- 分支 `feat/health-checks`（叠加在 `feat/rate-limit` 1c11ede 之后），HEAD 9fb72f1。
- 已 push 到 origin，CI 跟踪 9fb72f1。
- schema 仍为 **v6**（本轮无迁移）；E2E 数据已清理（users/websites/gateways/logs/stats = 0），
  测试进程与端口已释放。

## 本轮完成（上游健康检查 P-10/U-17 部分 / F-58）

### 实现
- 新增 `gateway/src/health.rs`：
  - `HealthCheckConfig`（websites.config.health_check）：enabled/path/interval_secs/timeout_secs/
    unhealthy_threshold/healthy_threshold，默认关闭；sanitize 校验 path 以 / 开头、interval 1..3600、
    timeout 夹到 interval 内、阈值 clamp 1..100。
  - `BackendHealth`：进程内 `RwLock<HashMap<key, Arc<BackendStatus>>>`，key = `site_id|backend_url`；
    healthy/failures/successes 为原子量；未探测过默认 healthy；`begin_check_at` 控制探测间隔；
    `retain` 清理配置中已删除的 backend；`unhealthy_count` 供指标。
  - 探测调度 `spawn`：每秒扫描 routing 中开启检查的站点，按 interval 对每个 backend 发 GET
    `<backend><path>`（timeout），status <500 视为健康；失败 WARN 日志。
  - `record_passive`：请求路径连接失败/超时计入失败（仅开启检查的站点）；恢复靠主动探测。
- 选择逻辑（state.rs `next_backend(context, health)`）：先按 path/rules 过滤候选，
  再过滤 unhealthy；若全部不健康回退完整集合；之后保持 weight/priority 语义，
  因此 priority 备用组在首选组不健康时自动生效。
- 接入：proxy（h1/h2 与 h3 buffered）、upgrade（WebSocket）在 upstream 成功/失败时记录被动状态；
  主循环启动 health 任务、停机 abort。
- 指标：`webgateway_health_check_success_total`、`failure_total`、`backends_unhealthy`。

### 实测（gateway 端口 18085/18086/18087；dead=18081，alive=18080）
- 开启健康检查的站点：6 次请求全部 200（dead 被摘除）。
- 未开启检查的对照站点：6 次请求 502/200 交替（说明 dead 平时会被选中）。
- 被动/主动都走同一 registry；日志 `upstream health check failed backend=http://127.0.0.1:18081/`。
- 指标：success 3、failure 3、backends_unhealthy 2、route_hosts 3。

### 文档
- `gateway.md` 2.2 指标清单、5.5 健康检查（配置 JSON/行为/限制）、8 未实现列表更新。
- `deployment.md` 限制表、告警规则（BackendsUnhealthy/RateLimitingActive）、上线检查表新增健康检查步骤。
- `issue.md` P-10/U-17/F-58、当前状态、优先级。

## 测试
- gateway 51（+6 health）、dashboard 25、shared 18 + 3 集成；fmt/clippy -D warnings 通过。
- CI 使用 `--locked`；本轮未新增依赖。

## 关键事实 / gotchas
- 健康状态**单节点**：多节点各自探测，不共享；无熔断/半开状态机。
- 探测使用 HTTP GET，可能在上游产生访问日志；4xx 视为健康（避免认证型 health endpoint 被误判）。
- 未配置 health_check 的站点完全走旧行为（不记录被动失败、不探测）。
- 全部 unhealthy 时回退完整候选集，仍可能 502，但不会全站拒绝。
- 探测任务每秒扫描一次 routing（克隆 Arc），站点数量很大时注意开销；当前实现未做批量并发上限。
- 测试端口 18085/18086/18087，指标 19101；测试数据已清理。

## 下一步（生产清单剩余）
1. HTTP/3 流式响应、缓存接入、停机 drain（B-04/P-01）。
2. 缓存正确性：Vary、single-flight、HEAD/304、stale-while-revalidate（B-09）。
3. 统计 total_ips/保留期/UTC 日（B-08/B-14）。
4. refresh token / 每用户 API Key / 服务令牌过期（B-30 剩余）。
5. 缓存持久化/多级缓存、WAF/CC、跨节点健康状态/熔断（U-23/U-07/P-10 剩余）。
6. B-21 分支整合（需用户确认）；CI DB E2E（U-31）依赖 PG18+uint128 镜像。
