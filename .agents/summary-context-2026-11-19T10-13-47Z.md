# Context Summary — 2026-11-19T10-13-47Z

## 当前状态
- 分支 `feat/gateway-routing` 已推送（路由 + gRPC h2c，CI 全绿）。
- 本轮新增访问日志，待提交（将新建 `feat/gateway-access-logs`）。
- beta 库 schema version 2；测试数据（配置 + 日志）已清理。

## 本轮完成
- `gateway/src/access.rs`：请求/响应主表 + 请求/响应 size 表写入；500ms 或 256 条批量事务。
- 请求/响应时间与 size `at_second` 全部用 PostgreSQL `now()`（主控时钟一致）。
- proxy 对 200/404/502/504 都记录；headers 存 JSON 对象；`$remote_addr` 用 get_request_ip 解析结果。
- E2E：GET/POST/404 落库；POST 5 字节、响应 12/9 字节；size 表行验证通过。
- 网关单测 26 个；文档更新。

## 未实现（下一步）
- 访问统计聚合（second/minute/daily）与保留期清理。
- WebSocket 升级透传、HTTP/3（QUIC）、特殊 RFC（CONNECT）。
- 缓存、IP/UA 拦截、上游 TLS、后端权重/主备。
- 日志限制：chunked 长度记为 0/NULL；退出时最后一批可能丢。

## 关键事实
- 日志表为周分区，主控预先创建未来两周；写入依赖当前周分区存在。
- 本机工具链 `.tools/`（gitignored）；beta 库连接串不进仓库。
