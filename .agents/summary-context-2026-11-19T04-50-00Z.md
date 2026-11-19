# Context Summary — 2026-11-19T04-50-00Z

## 当前状态
- 分支 `feat/database` 已有主控与数据库层；本轮新增网关，尚未提交（将新建 `feat/gateway`）。
- 网关功能已实现并在 beta 库 E2E 验证通过；测试数据已清理。

## 本轮完成
- `gateway` crate：等待主控 schema，`gateways` online/心跳/offline，10s 配置全量同步。
- 动态端口监听（配置端口 ∪ `websites.ports`）、HTTP/HTTPS、SNI 证书（DB PEM + 默认自签）、ALPN h2/http1.1。
- Host 精确/通配符路由、后端 `match_path` + 轮询、`X-Forwarded-*`、节点 header、404/502/504、60s 超时。
- 单元测试 9 个；E2E：HTTP 200、HTTPS h2 200、未知 Host 404、心跳更新、SIGTERM offline、`gateways_online` 归零。

## 关键事实
- 后端只支持 `http://` 上游；`https://` 跳过并告警。
- 网关不执行 DDL；schema 由主控迁移。
- beta 库 `webgateway_beta_v2` schema version 1；测试后 websites/gateways 表已清空。
- 本地工具链 `.tools/`（rustup/cargo/libpq）已 gitignore；代理 `http://192.168.2.253:7890`。

## 下一步
1. 提交 `feat/gateway` 并推送，确认 CI（Rust job 会编译 gateway + 跑单测）。
2. 访问日志写入（请求/响应/size 表）与秒/分/日聚合、保留期。
3. 网关缓存（主控下发：大小/类型/ETag/TTL）与 IP/UA 拦截。
4. NOTIFY 快速同步、WebSocket/上游 TLS、CRUD/API/认证/ACME/前端。
