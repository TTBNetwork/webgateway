# Context Summary — 2026-11-19T10-33-21Z

## 当前状态
- 分支 `feat/gateway-upgrades` 已推送（WebSocket + CONNECT，CI 全绿）。
- 本轮新增 HTTP/3（QUIC），待提交（将新建 `feat/gateway-http3`）。
- beta 库 schema version 2；测试数据已清理。

## 本轮完成
- 依赖：quinn / h3 / h3-quinn；`[server].http3_ports` / `GATEWAY_HTTP3_PORTS` 开启 UDP 监听。
- QUIC TLS 1.3 + ALPN h3，复用 `CertStore` 的 SNI 解析（默认自签证书可用）。
- h3 请求/响应缓冲；`proxy::handle_buffered` 用 buffered HTTP/1.1 client 转发上游并写访问日志（HTTP/3.0）。
- E2E：aioquic GET `/h3` → 200 `http3-upstream-ok`；日志 http_version=HTTP/3.0、body 18。

## 未实现
- HTTP/3 流式 body、同连接并发请求、0-RTT / 连接迁移。
- WebSocket/gRPC over h3、h2 extended CONNECT、上游 TLS。
- 统计聚合、缓存、IP/UA 拦截。

## 关键事实
- 本地验证用 `.tools/venv`（aioquic 1.8.0）+ `.tools/uv-cache`，均 gitignored。
- HTTP/3 端口必须显式配置；UDP 与 TCP 可同端口共存。
- 本机工具链 `.tools/`；beta 库连接串不进仓库。
