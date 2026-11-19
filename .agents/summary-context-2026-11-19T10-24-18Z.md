# Context Summary — 2026-11-19T10-24-18Z

## 当前状态
- 分支 `feat/gateway-access-logs` 已推送（访问日志，CI 全绿）。
- 本轮新增 WebSocket 与 CONNECT，待提交（将新建 `feat/gateway-upgrades`）。
- beta 库 schema version 2；测试数据已清理。

## 本轮完成
- WebSocket：`Upgrade: websocket` 检测 → 站点/后端路由 → 上游 h1 握手（`with_upgrades`）→ 101 + 双向桥接。
- CONNECT：authority host 命中站点才允许；连接 host:port 后 200 + 原始隧道；否则 403。
- 访问日志记录 101/200；E2E 验证 WS 回显与 CONNECT 隧道。
- 网关单测 28 个；fmt/clippy 通过。

## 未实现
- HTTP/3（QUIC + h3 + UDP + ALPN h3）：需要把代理核心与 hyper 请求类型解耦，独立改动。
- HTTP/2 extended CONNECT / WebSocket over h2、上游 TLS、统计聚合、缓存、IP/UA 拦截。

## 关键事实
- WS 上游必须是 http:// 明文；CONNECT 只做 TCP 透传，不做 TLS MITM。
- 升级连接 body_length 记 0；访问日志会记录 101/200。
- 本机工具链 `.tools/`（gitignored）；beta 库连接串不进仓库。
