# Context Summary — 2026-11-20T04-23-32Z

## 当前状态
- 分支 `feat/connect-alt-svc`（叠加在 `feat/limits-hardening` 之后）。
- schema 仍为 **v6**（本轮无迁移）；CONNECT 测试数据已清理
  （users/websites/certificates/gateways/logs/stats = 0），测试进程已停止。

## 本轮完成（B-07 + B-12）

### CONNECT
- `upgrade.rs::handle_connect` 在解析 authority 前创建 RequestLog；
  400/403/502/504/200 全部 log_failure/log_event。
- 新增配置 `[server].connect_ports`（默认 [443, 80]）；`GATEWAY_CONNECT_PORTS` 覆盖；
  空列表拒绝全部；端口不在列表 → 403。
- 站点不匹配 → 403，且现在也写访问日志。
- `AppState` 保存 `connect_ports: Arc<[u16]>`。
- 实测：200/403/403 三条 CONNECT 访问日志（host 为 authority，body_length 9）。

### HTTP/3 Alt-Svc
- `AppState.alt_svc` 在 `h3_port = http3_ports.first()` 时构建
  `h3=":<port>"; ma=86400`。
- `listeners::serve` 在 scheme == "https" 时为所有响应插入 `Alt-Svc`；
  明文 HTTP 与 h3 自身不加。
- 单测：`formats_alt_svc_for_h3`。

### 其它
- `AppState::new` 的参数合并为 `StateLimits`（upstream_ca + cache/body/connect/h3），
  通过 clippy too_many_arguments 检查。
- 未新增 schema 迁移。

## 关键事实
- CONNECT 白名单默认只允许 443/80，是**破坏性默认**：需要其他端口时必须配置
  `connect_ports` 或 `GATEWAY_CONNECT_PORTS`。
- CONNECT 访问日志的 host/path 都是 authority（如 `127.0.0.1:18443`）；
  403 行的 website_id 为 NULL（站点未命中或端口不允许）。
- Alt-Svc 只在 TLS h1/h2 响应注入；h3 响应不加；明文 HTTP 不加。
- Alt-Svc 的 UDP 端口取 `http3_ports[0]`；多端口时只广播第一个。

## 测试
- gateway 37、dashboard 10、shared 14 + 3 集成；fmt/clippy 通过。
- E2E 使用 raw socket CONNECT + Python http.server 18443；已清理站点/网关/日志。

## 下一步
1. B-04 剩余：h3 缓存/流式、h3 drain；站点级限制/超时。
2. B-15 CSP/localStorage；refresh token / API Key / 服务令牌轮换。
3. B-08 统计偏差、B-09 剩余（single-flight/304/HEAD）、B-21 分支合并。
4. 证书：ZeroSSL EAB 实测、通配符、私钥加密。
