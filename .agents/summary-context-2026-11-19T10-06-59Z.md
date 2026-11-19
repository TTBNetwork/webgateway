# Context Summary — 2026-11-19T10-06-59Z

## 当前状态
- 分支 `feat/gateway-proxy` 已推送（PROXY protocol + 客户端 IP 模式，CI 全绿）。
- 本轮新增路由规则与 gRPC h2c 传输，待提交（将新建 `feat/gateway-routing`）。
- beta 库 schema version 2；测试数据已清空。

## 本轮完成
- `MatchRules`：path（glob）、user_agent（忽略大小写 glob）、query（全部键匹配/空值存在）、method；
  所有条件 AND；命中后端轮询；无命中回退全部。
- `MatchContext` 从请求的 path/query/method/UA 构造；`Backend` 增加 `match` 配置解析。
- gRPC：`AppState.grpc_client` 为 `http2_only` 客户端；content-type `application/grpc*` 走 h2c，
  保留 `TE: trailers`，不套 60s 超时。
- E2E：path/UA/query/method 组合路由分别命中 9001/9002；gRPC 仅做检测单测（无真实 gRPC 服务端）。
- 网关单测 23 个；文档更新。

## 未实现（下一步）
- WebSocket（upgrade 桥接）、HTTP/3（QUIC）、特殊 RFC（CONNECT 等）。
- 访问日志/统计、缓存、IP/UA 拦截、上游 TLS、后端权重/主备。
- gRPC over TLS（需要上游 TLS 支持；当前只支持 h2c 明文）。

## 关键事实
- 路由规则示例见 gateway.md 第 3 节；gRPC 行为见第 5 节。
- 本地工具链 `.tools/`（gitignored）；beta 库连接串不进仓库。
