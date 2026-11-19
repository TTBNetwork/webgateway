# Context Summary — 2026-11-19T05-16-55Z

## 当前状态
- 分支 `feat/gateway-notify` 已推送（NOTIFY 同步/站点 headers/节点哈希；CI 全绿）。
- 本轮新增 PROXY protocol + 客户端 IP 模式，待提交（将新建 `feat/gateway-proxy`）。
- beta 库 schema version 2；websites/gateways 测试数据已清空。

## 本轮完成
- `proxy_protocol.rs`：v1 文本 / v2 二进制解析，读取发生在 TLS/HTTP 之前。
- 站点 `get_request_ip`：`raw` / `proxyProtocol` / `xForwardedFor(index)` / `xRealIP`；
  `$remote_addr` 与上游头使用解析后的客户端 IP；未启用 proxyProtocol 的站点仍不信任头。
- E2E：无头 → 127.0.0.1；v1 → 198.51.100.7；v2 → 203.0.113.9。
- 文档 gateway.md/AGENT.md/changelog 更新；网关单测 19 个。

## 未实现（下一阶段，已写入 gateway.md 第 8 节）
- 上游传输：当前 HTTP/1.1；gRPC 需要 h2/h2c 客户端 + trailers；WebSocket/HTTP3/特殊 RFC 待做。
- 路由分配：当前 Host + match_path；计划支持 path/UA/query/method 规则（示例已写入文档）。
- 访问日志/统计、缓存、IP/UA 拦截、上游 TLS、后端权重/主备。

## 关键事实
- ProxyProtocol 头总是在 TLS 前消费；是否信任由站点 get_request_ip 决定。
- XFF 索引 0 = 最左（原始客户端），越界取最后一个。
- 本地工具链 `.tools/`（gitignored）；beta 库连接串不进仓库。
