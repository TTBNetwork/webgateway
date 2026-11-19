# Context Summary — 2026-11-19T13-58-54Z

## 当前状态
- 分支 `feat/upstream-tls` 已推送（上游 TLS，CI 全绿）。
- 本轮新增后端权重/优先级 + HTTP/3 并发，待提交（将新建 `feat/backend-weights`）。
- beta 库 schema version 2；测试数据已清理。

## 本轮完成
- `Backend` 增加 `weight` / `priority`；选择时取最小 priority 组，组内按权重轮询；
  更高 priority 为备用组，当前无失败自动切换。
- HTTP/3 `serve_connection` 对每个 resolver 独立 spawn，同连接多流并发；body 仍缓冲。
- E2E：权重 40 次 30/10、备用 0；HTTP/3 同连接 5 请求全 200、4/1；aioquic 验证。
- 网关单测 33 个。

## 未实现
- HTTP/3 流式 body、0-RTT/连接迁移；h2 extended CONNECT/WebSocket over h2。
- 失败自动切换/健康检查、上游客户端证书、缓存持久化/指标。
- IP/UA 拦截（按用户要求暂不做）。

## 关键事实
- 权重轮询用 selector 取模总权重，简单确定；priority 是静态备用组。
- h3 并发仅针对请求处理；每请求 body 仍完整缓冲。
- 本机工具链 `.tools/`；aioquic 在 `.tools/venv`（gitignored）。
