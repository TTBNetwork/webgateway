# Context Summary — 2026-11-19T05-05-01Z

## 当前状态
- 分支 `feat/gateway` 已有网关基础版；本轮增强待提交（将新建 `feat/gateway-notify`）。
- beta 库 schema version 2；测试数据已清理（websites/gateways 均为 0 行）。

## 本轮完成
- 配置同步：LISTEN/NOTIFY 驱动，迁移 0002 增加 DELETE 通知；重连用水位 `(count, max(updated_at))` 对比；
  移除 10s 全量轮询。
- 请求头：`websites.config.headers` set/remove + 占位符 `$remote_addr` / `$scheme` / `$host`；
  未配置用默认四个头；逐跳头始终剔除。
- 节点 header：`<location>-<fnv1a64(location+machine_id) 16hex>`，例如 `cn-shanghai-18411588dcb04048`。
- 迁移 0002 已应用（主控）；shared/gateway 测试通过；E2E 验证 NOTIFY 增/删、headers、节点哈希。

## 关键事实
- 后端仅支持 http:// 上游；WebSocket / gRPC / HTTP3 / 特殊 RFC 仍待实现（gateway.md 第 8 节）。
- 网关仍然不执行 DDL；schema 版本 2。
- 本地验证流程：beta 库 + 本地上游 + `target/debug/gateway`；工具链在 `.tools/`（gitignored）。

## 下一步
1. 提交 `feat/gateway-notify`，推送并确认 CI。
2. 访问日志/统计、缓存、IP/UA 拦截。
3. 后端协议：WebSocket → gRPC（h2/h2c + trailers）→ HTTP/3（QUIC）→ 特殊 RFC。
