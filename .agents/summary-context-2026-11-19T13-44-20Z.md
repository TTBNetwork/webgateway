# Context Summary — 2026-11-19T13-44-20Z

## 当前状态
- 分支 `feat/gateway-cache` 已推送（缓存，CI 全绿）。
- 本轮新增上游 TLS，待提交（将新建 `feat/upstream-tls`）。
- beta 库 schema version 2；测试数据已清理。

## 本轮完成
- hyper-rustls HttpsConnector 替换 HttpConnector；http/https 后端都可路由。
- 证书：webpki 根 + `[upstream_tls].ca_file` / `GATEWAY_UPSTREAM_CA_FILE`。
- 普通/gRPC/buffered 三个客户端共用 connector；gRPC over TLS 可用。
- E2E：自建 CA + HTTPS 上游（localhost:9443）→ 200 upstream-https-ok。
- 网关单测 32 个；fmt/clippy 通过。

## 未实现
- IP/UA 拦截（按用户要求暂不做）、h2 extended CONNECT、HTTP/3 流式/并发。
- 上游客户端证书、后端权重/主备、缓存持久化/指标。

## 关键事实
- hyper-rustls 的 with_tls_config 不允许预置 ALPN（会 panic），由 builder 配置。
- https 后端依赖 SNI/主机名校验；自签测试通过 ca_file 信任。
- 本机工具链 `.tools/`；beta 库连接串不进仓库。
