# Context Summary — 2026-11-20T10:32:45Z

## 当前状态
- 分支 `feat/stats-accuracy`（叠加在 `feat/cache-vary` 88b0191 之后），HEAD 4a0450b。
- 已 push 到 origin，CI 跟踪 4a0450b（最终回复附 run）。
- schema 仍为 **v6**（本轮无迁移）；E2E 数据已清理（users/raw logs/stats = 0），测试端口释放。

## 本轮完成（统计 B-08 / 时区 B-14，F-61）

### total_ips 去重
- `MINUTE_ROLLUP`：删除秒行前，按本次归并窗口从 `access_request_logs` 重新计算
  `COUNT(DISTINCT remote_addr)`；秒聚合值只在原始日志已过期时作为回退。
- `DAILY_ROLLUP`：跨天归并时同样按天从原始日志重算 distinct IP；回退为分钟聚合值。
- `BULK_MINUTE_ROLLUP` 原本就是按分钟从原始日志 distinct。
- 集成测试新增断言：同分钟 3 请求/2 IP → minute total_ips=2；同天跨两分钟 3 请求/2 IP →
  daily total_ips=2。

### 保留期配置
- `[stats].raw_retention_days` / `STATS_RAW_RETENTION_DAYS`：默认 7，范围 **7..90**
  （低于 7 会 WARN 并夹到 7，因为周分区只能整周 DROP；SQL 函数本身也要求 >=7）。
- 分区 DROP、边界逐行 DELETE、四张访问日志表清理都使用该值。
- 实测：`STATS_RAW_RETENTION_DAYS=3` → 日志 `clamped the raw access-log retention`，rollup 正常。

### 面板时区（B-14）
- `GET /api/summary?tz_offset_minutes=<分钟>`：-840..840，夹紧；今日 requests/responses/
  4xx/5xx/sizes 直接从原始访问日志按 **本地日** 窗口计算（避免 UTC 日界与跨日聚合），
  响应回显 `timezone_offset_minutes`。
- 前端 `api.summary()` 自动带 `-new Date().getTimezoneOffset()`。
- 实测：把 1 请求/1 响应(503)/12、34 字节放在“UTC 昨天、+08 今天”：
  offset=480 → 1/1/1 且 sizes 12/34；offset=0 → 0；5000 → 840；非数字 → 400。

## 测试
- shared 18 → 19（+保留期 clamp）；dashboard 25 → 26（+时区 clamp）；gateway 53；
  shared 3 个集成测试通过（含新 distinct IP 断言）；fmt/clippy 通过。
- 前端 format/lint/type-check/build 通过。
- CI 使用 `--locked`，未新增依赖。

## 关键事实 / gotchas
- 面板 summary 现在扫描本地日窗口的原始日志（保留期内），流量图表仍用聚合表；
  高流量实例若 summary 变慢，可后续加小时级聚合表/缓存。
- 原始日志保留期最低 7 天（周分区约束），秒/分/日表保留期仍为 1h/1d/长期（未配置化）。
- sizes 仍取日志 Content-Length：chunked 请求为 0、响应 NULL（B-08 剩余）。
- 时区偏移只看浏览器；多用户/站点时区未做。

## 下一步（最后 1 轮 / 生产清单收尾）
1. 汇总最终生产清单到 deployment.md（P0/P1/P2 状态、操作步骤、未解决项）。
2. 可选项：B-30 刷新令牌/API Key/服务令牌过期、单点负载均衡缓存 single-flight、
   h3 缓存、WAF、CI DB E2E。
3. B-21 分支整合仍需用户确认方向。
