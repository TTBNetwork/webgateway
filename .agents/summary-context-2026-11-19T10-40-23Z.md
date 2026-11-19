# Context Summary — 2026-11-19T10-40-23Z

## 当前状态
- 分支 `feat/gateway-http3` 已推送（HTTP/3，CI 全绿）。
- 本轮新增统计 rollup + 保留期，待提交（将新建 `feat/stats-rollup`）。
- beta 库 schema version 2；测试数据已清理。

## 本轮完成
- `dashboard/backend/src/stats.rs`：每 10s 滚动聚合最近 90s（2s lag）的秒级统计；事务级 advisory lock。
- 秒→分（>1h）、分→日（跨天）原子归并；原始日志 7 天保留，每小时 prune。
- E2E：3 条请求/响应 → 秒级请求 30/响应 150；旧秒→分 70/700；旧分→日 90/900；旧行删除。
- 文档 database.md 5.4、gateway.md 已更新。

## 未实现
- 缓存、IP/UA 拦截、上游 TLS、h2 extended CONNECT。
- HTTP/3 流式/同连接并发、0-RTT/连接迁移。
- 统计保留期配置化（当前硬编码 7d/1h/1d）。

## 关键事实
- 统计由主控执行（单实例语义），不在网关；多主控靠 advisory lock 互斥。
- total_ips 跨秒会重复计数；backend_error=502/504。
- 本机工具链 `.tools/`；beta 库连接串不进仓库。
