# WebGateway v2 生产部署指南

本文面向运维/部署：说明当前代码已具备的生产能力、必需的配置与已知限制。
代码位置与行为以仓库为准；标注 ⚠️ 的条目是**上线前必须确认**的限制。

## 1. 部署拓扑

```text
                    TLS 反向代理（nginx / Caddy）
外部用户 ────────────────► dashboard :3000（控制面 + 面板 + /api）
                                   │
                                   ▼
                            PostgreSQL 18（uint128/btree_gin）
                                   ▲
用户域名 ──► gateway :80/:443 (+UDP 443 h3) ──► 源站
```

- **dashboard（主控）必须是单实例**：它独占 schema 迁移、统计 rollup、证书续期与分区维护；
  多实例只会增加 advisory lock 竞争，登录限流与服务令牌计数也不共享。
- **面板与 `/api` 必须同源**：登录会话是 HttpOnly Cookie（SameSite=Strict），跨源时浏览器不会携带。
  dashboard 自带静态托管（`DASHBOARD_STATIC_DIR`）即可满足；若由 nginx 托管面板，必须把 `/api` 与
  `/health` 反代到同一个 origin。
- **gateway 需要绑定 80/443**：用 systemd `AmbientCapabilities=CAP_NET_BIND_SERVICE`，不要用 root 运行。
- HTTP/3 需要 UDP 端口（默认同 443），并在防火墙放行 UDP。

## 2. 构建

```bash
# Rust（推荐 release）
cargo build --release -p dashboard -p gateway

# 面板
cd dashboard/frontend
pnpm install --frozen-lockfile
pnpm build          # 产物在 dist/
```

部署目录建议：

```text
/opt/webgateway/bin/{dashboard,gateway}
/opt/webgateway/panel/            # dashboard/frontend/dist
/etc/webgateway/{dashboard.env,gateway.env,gateway.toml}
```

## 3. 数据库

- PostgreSQL 18 + `uint128`、`btree_gin` 扩展（镜像见仓库根目录 `postgresql-dockerfile`）。
  扩展由 dashboard 在迁移开始时创建，数据库用户需要建扩展权限。
- dashboard 首次启动会自动迁移并把版本写入 `schema_state`；**gateway 不执行 DDL**，
  启动时等待主控发布 schema 版本，因此必须先启动 dashboard。
- 升级顺序：备份 → 启动新 dashboard（迁移）→ 滚动重启 gateway。
  迁移目前不可回退，升级前务必 `pg_dump -Fc`：

```bash
pg_dump -Fc --no-owner --dbname="$DATABASE_URL" -f webgateway-$(date -u +%Y%m%dT%H%M%SZ).dump
```

- 访问日志按周分区自动续建与 DROP（保留 7 天）；统计保留策略仍是硬编码（B-08）。

## 4. dashboard 配置

配置文件默认读取进程工作目录下的 `config.toml`（可忽略），环境变量优先。示例见
`dashboard/backend/config.example.toml`。

| 变量 | 必填 | 说明 |
|---|---|---|
| `DATABASE_URL` | 是 | PostgreSQL 连接串 |
| `DASHBOARD_API_PORT` | 否 | 监听端口，默认 3000 |
| `DASHBOARD_STATIC_DIR` | 推荐 | 构建后的面板目录；设置后 dashboard 直接托管面板与 SPA 回退 |
| `DASHBOARD_METRICS_PORT` | 推荐 | Prometheus 指标端口；不设则不启用 |
| `STATS_RAW_RETENTION_DAYS` | 否 | 原始访问日志/周分区保留天数（7..90，默认 7） |
| `DASHBOARD_METRICS_BIND` | 否 | 指标监听地址，默认 `127.0.0.1` |
| `DASHBOARD_API_TOKENS` | 推荐 | 服务令牌，逗号分隔；轮换期间可同时配置新旧两个 |
| `DASHBOARD_API_TOKEN` | 兼容 | 单个服务令牌（已弃用，仍会合并到列表） |
| `DASHBOARD_TOKEN_EXPIRES` | 否 | 面板会话秒数，默认 604800，范围 300..2592000 |
| `DASHBOARD_ADMIN_TOTP_SECRET` | 首次 | 空库创建默认管理员时使用；不设则随机生成并只打印一次 |
| `DASHBOARD_ADMIN_USERNAME` | 否 | 默认管理员用户名，默认 `admin` |
| `DASHBOARD_JWT_SECRET` | 否 | 默认管理员的 JWT 密钥；不设则随机生成后入库 |
| `SECRET_ENCRYPTION_KEY` | 生产必填 | 32 字节密钥（64 hex 或 base64），加密数据库中的控制面 secret |
| `SECRET_ENCRYPTION_KEY_PREVIOUS` | 轮换时 | 旧密钥，仅用于解密过渡 |
| `ACME_EMAIL` | 否 | 证书申请联系邮箱的兜底值 |
| `RUST_LOG` | 否 | 建议 `dashboard=info,shared=info` |

首次启动要点：

1. 空 `users` 表会创建默认管理员；TOTP secret 只在 WARN 日志中出现一次。
   生产建议先设置 `DASHBOARD_ADMIN_TOTP_SECRET`（base32）再启动，直接保存到密码管理器。
2. `DASHBOARD_API_TOKENS` 请生成高熵随机值（例如 `openssl rand -hex 32`），不要复用开发令牌。
3. 密钥加密见 4.1；未设置 `SECRET_ENCRYPTION_KEY` 时 secret 仍会明文入库。

### 4.1 密钥静态加密

- 设置 `SECRET_ENCRYPTION_KEY` 后，以下字段以 AES-256-GCM 加密存储（`enc:v1:` 前缀）：
  `users.totp_secret`、`users.jwt_secret`、`users_client_secrets.secret`、`certificates.private_key`、
  `configurations` 中的 ACME account key、`dns_providers.provider` 的 `secret_id`/`secret_key`。
  `certificates.fullchain` 是公开证书链，保持明文。
- dashboard 每次启动会把仍是明文的存量数据加密（幂等，日志 `encrypted plaintext secrets count=N`）。
- **上线/升级顺序**：先在 dashboard 和**所有 gateway** 配置同一个密钥，再重启 dashboard 触发加密。
  gateway 没有密钥时会打印 `cannot decrypt the certificate key` 并跳过该证书，回落到默认自签证书。
- **轮换**：新密钥放 `SECRET_ENCRYPTION_KEY`，旧密钥放 `SECRET_ENCRYPTION_KEY_PREVIOUS`；
  旧密文仍可解密，但不会自动重加密（后续任务）。
- 密钥只存在环境变量中，不写数据库/仓库；请纳入备份与密钥管理系统。密钥丢失会导致登录 500、
  证书无法加载，且无法恢复。

## 5. TLS 与反向代理

dashboard 只监听明文 HTTP，Cookie 的 `Secure` 与 HSTS 依赖代理发送 `X-Forwarded-Proto: https`。
nginx 最小示例：

```nginx
server {
    listen 443 ssl;
    http2 on;
    server_name panel.example.com;

    ssl_certificate     /etc/letsencrypt/live/panel.example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/panel.example.com/privkey.pem;

    client_max_body_size 2m;

    location / {
        proxy_pass http://127.0.0.1:3000;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto https;
    }
}
```

- 若使用 `DASHBOARD_STATIC_DIR`，nginx 只需把全部请求反代到 dashboard，面板、SPA 回退、
  安全头（CSP/HSTS/nosniff）都由 dashboard 负责。
- 若由 nginx 自行托管 `dist/`，需要自己配置 `try_files $uri /index.html`、`/api` 反代，
  以及等价安全响应头；否则 B-27 的安全头只覆盖 API。
- 面板构建产物带 hash 的资源可长期缓存，`index.html` 必须 `no-store`。

## 6. systemd 示例

`/etc/systemd/system/webgateway-dashboard.service`：

```ini
[Unit]
Description=WebGateway dashboard
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=webgateway
Group=webgateway
WorkingDirectory=/opt/webgateway
EnvironmentFile=/etc/webgateway/dashboard.env
ExecStart=/opt/webgateway/bin/dashboard
Restart=on-failure
RestartSec=3
KillSignal=SIGTERM
TimeoutStopSec=45
RuntimeDirectory=webgateway
RuntimeDirectoryMode=0750
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
ReadOnlyPaths=/opt/webgateway

[Install]
WantedBy=multi-user.target
```

`/etc/systemd/system/webgateway-gateway.service`：

```ini
[Unit]
Description=WebGateway edge node
After=network-online.target webgateway-dashboard.service
Wants=network-online.target

[Service]
Type=simple
User=webgateway
Group=webgateway
WorkingDirectory=/opt/webgateway
EnvironmentFile=/etc/webgateway/gateway.env
ExecStart=/opt/webgateway/bin/gateway
AmbientCapabilities=CAP_NET_BIND_SERVICE
CapabilityBoundingSet=CAP_NET_BIND_SERVICE
Restart=on-failure
RestartSec=3
KillSignal=SIGTERM
TimeoutStopSec=30
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
ReadOnlyPaths=/opt/webgateway

[Install]
WantedBy=multi-user.target
```

dashboard 收到 SIGTERM/Ctrl-C 后停止 accept，最多等待 30 秒；gateway 同样在 10 秒内 drain 在途
HTTP/TCP 与 HTTP/3 请求、flush 访问日志并标记 offline。

## 7. gateway 配置

`gateway.toml` 示例见 `gateway/config.example.toml`；环境变量覆盖：

| 变量 | 说明 |
|---|---|
| `DATABASE_URL` | 必需，与主控同一个库 |
| `GATEWAY_PORTS` | 逗号分隔的 TCP 监听端口，默认 80,443 |
| `GATEWAY_HTTP3_PORTS` | 逗号分隔的 UDP 端口；空表示禁用 HTTP/3 |
| `GATEWAY_BIND` | 监听地址，默认 0.0.0.0 |
| `GATEWAY_NODE_NAME` / `_COUNTRY` / `_CITY` / `_ADDRESS` | 节点元数据，面板展示与 `X-WebGateway-Node` |
| `GATEWAY_CONNECT_PORTS` | CONNECT 允许的目标端口，默认 443,80；空列表全部拒绝 |
| `GATEWAY_CACHE_MAX_SIZE` | 每节点缓存上限（字节），默认 16 MiB |
| `GATEWAY_MAX_CONNECTIONS` | TCP 并发连接上限，默认 4096 |
| `GATEWAY_MAX_HEADER_BYTES` | 请求头上限，默认 32 KiB（最小 8 KiB） |
| `GATEWAY_MAX_REQUEST_BODY` / `GATEWAY_MAX_RESPONSE_BODY` | 请求/响应体上限，默认 64 MiB |
| `GATEWAY_UPSTREAM_CA_FILE` | 额外信任的上游 CA PEM |
| `GATEWAY_METRICS_PORT` | Prometheus 指标端口；不设则不启用 |
| `GATEWAY_METRICS_BIND` | 指标监听地址，默认 `127.0.0.1` |
| `GATEWAY_RATE_LIMIT_RPS` | 每客户端 IP 每秒请求数；>0 时启用限流 |
| `GATEWAY_RATE_LIMIT_BURST` | 令牌桶容量，默认 `ceil(rps)` |
| `GATEWAY_RATE_LIMIT_MAX_TRACKED` | 跟踪的客户端 IP 上限，默认 100000 |
| `SECRET_ENCRYPTION_KEY` | 与 dashboard 相同的密钥；读取已加密的证书私钥时必须设置 |
| `SECRET_ENCRYPTION_KEY_PREVIOUS` | 轮换期间用于解密旧密文 |
| `RUST_LOG` | 建议 `gateway=info,shared=info` |

同一台机器跑多个 gateway 时必须使用不同的 `machine_id`（不同用户/容器），否则会争用同一节点记录与端口。

## 8. 首次上线检查表

1. 数据库建库、扩展权限、`pg_dump` 基线备份。
2. 生成 `SECRET_ENCRYPTION_KEY`（`openssl rand -hex 32`）并分发到 dashboard 与所有 gateway。
3. 部署 dashboard + `panel/`，配置 `DASHBOARD_STATIC_DIR`、`DASHBOARD_API_TOKENS`、TOTP secret。
4. 启动 dashboard，`curl -fsS https://panel.example.com/health` 返回 `status=ok`、schema 版本符合预期。
5. 浏览器登录面板（用户名 + TOTP），确认会话 Cookie 为 HttpOnly/Secure。
6. 面板中创建 DNS provider、证书（手动上传或 ACME）、站点与上游。
7. 启动 gateway（同样的 `SECRET_ENCRYPTION_KEY`），确认 `gateways.status = online` 且 10 秒内有心跳。
8. 域名解析指向 gateway，验证 HTTPS 证书、路由、访问日志与统计入库。
9. 打开站点缓存后验证 MISS→HIT；验证 413/431 等上限行为。
10. 给关键站点配置 `health_check`，故意停一个后端，确认请求只落到健康后端且
    `webgateway_backends_unhealthy` 归零/恢复。
11. 做一次 `systemctl restart webgateway-dashboard` 和 `kill -TERM` gateway，确认无异常退出。
12. 在主机运行 `webgateway-mnt admin-totp`（Docker：`docker compose --profile tools run --rm mnt admin-totp`）
    确认能取到当前 TOTP；首次绑定用 `--secret` 获取 secret/otpauth URI。

## 9. 已知生产限制（上线前必须评估）

| 级别 | 限制 | 说明 |
|---|---|---|
| — | 发布准备 | 主线已整理：master = 原 v2 + 全部 feature 分支（de1da22），旧原型 = v1；按第 8 节完成环境/密钥/域名切换即可发布 |
| P1 | 密钥依赖环境变量 | 未设置 `SECRET_ENCRYPTION_KEY` 时仍明文入库；密钥丢失不可恢复；无自动重加密（B-31/P-13） |
| P2 | 告警在本地 | 指标由本地 Prometheus 抓取/告警（按用户决定，不内置 Alertmanager/集中告警）；配置见第 11 节 |
| P2 | 认证增强 | 无 refresh token、每用户 API Key、服务令牌过期/自动轮换（B-30 剩余） |
| P2 | 健康状态为单节点 | 站点级主动探测 + 被动失败标记已实现；无跨节点健康状态、无熔断/半开（P-10/U-17 部分修复） |
| P3 | WAF/CC 暂缓 | 已有每客户端 IP 令牌桶限流（429 + Retry-After）；WAF/CC/UA 规则按用户要求暂缓，全局配额未实现 |
| P2 | HTTP/3 请求体仍缓冲 | 响应已流式、已参与停机 drain；请求体仍整体缓冲（上限 max_request_body_bytes），h3 无缓存（B-04/P-01 部分修复） |
| P2 | 缓存增强 | `Vary`/HEAD/`If-None-Match` 已支持；无 single-flight、`If-Modified-Since`、stale-while-revalidate、持久化（B-09 部分修复） |
| P2 | 登录限流单实例 | 进程内计数，多实例不共享（B-29） |
| P2 | CI 无 DB E2E | shared 集成测试在 CI 中跳过；h3/WS/gRPC 仅本地验证（U-31/B-22） |
| P2 | 版本与分页 | 面板版本/schema 硬编码、列表无服务端分页（B-17/B-18） |
| P2 | 统计 sizes | 今日统计/时区、`total_ips`、原始保留期已修；请求/响应 sizes 仍依赖 Content-Length（chunked 为 0/NULL），需补流式路径的 chunked 实际字节统计（v1 有该能力，下一批次） |
| P3 | 协议扩展 | h2 extended CONNECT / WebSocket over h2、gRPC-Web、h3 请求体流式（U-13/U-15） |
| P3 | 依赖审计 | 无 cargo audit / 前端依赖审计（B-23） |

## 10. 运维手册（当前能力）

- 健康检查：`GET /health`（数据库 + schema 版本）。节点在线状态看 `gateways.status` 与
  `last_seen_at`（心跳 10s，面板按 30s 判定在线）。
- 日志：stdout/stderr 交给 journald；`RUST_LOG` 控制级别；访问日志写库，不落盘。
- 备份：`pg_dump -Fc` 全库；恢复后先启动 dashboard 校验 schema。
- 证书续期：dashboard 调度器在到期前 30 天自动续期（`auto_renew` 行）；手写上传证书不自动续期。
- 缓存：任意配置/证书 NOTIFY 会清空节点缓存，属预期行为。
- 升级：先 dashboard 后 gateway；gateway 滚动重启会 drain HTTP 与 HTTP/3 在途请求（最多 10s）。
- 指标：dashboard 与 gateway 各自在独立回环端口暴露 `/metrics`，见第 11 节。
- 限流：`GATEWAY_RATE_LIMIT_RPS` 启用每客户端 IP 令牌桶；超限返回 429 + `Retry-After`，
  计数见 `webgateway_rate_limited_total`。单节点计数，多节点需在边缘外做全局配额。
## 11. 监控与告警

两个进程都暴露 Prometheus 文本指标。指标监听默认只绑定回环地址，不经过公网 listener：

| 服务 | 环境变量 | 默认 | 说明 |
|---|---|---|---|
| dashboard | `DASHBOARD_METRICS_PORT` | 关闭 | 设置后启用 `/metrics` |
| dashboard | `DASHBOARD_METRICS_BIND` | `127.0.0.1` | 指标监听地址 |
| gateway | `GATEWAY_METRICS_PORT` | 关闭 | 设置后启用 `/metrics` |
| gateway | `GATEWAY_METRICS_BIND` | `127.0.0.1` | 指标监听地址 |

Prometheus 抓取示例（与实例同机；跨机请用 SSH 隧道或内网，不要把端口暴露到公网）：

```yaml
scrape_configs:
  - job_name: webgateway-dashboard
    static_configs:
      - targets: ["127.0.0.1:9103"]
  - job_name: webgateway-gateway
    static_configs:
      - targets: ["127.0.0.1:9102"]
```

主要指标：

- gateway（数据面）：
  - 请求：`webgateway_requests_total`、`webgateway_http3_requests_total`、
    `webgateway_websocket_requests_total`、`webgateway_connect_requests_total`；
  - 缓存：`webgateway_cache_hits_total`、`webgateway_cache_misses_total`、`webgateway_cache_expired_total`、
    `webgateway_cache_stores_total`、`webgateway_cache_evictions_total`、`webgateway_cache_rejected_total`、
    `webgateway_cache_clears_total`、`webgateway_cache_entries`、`webgateway_cache_bytes`、
    `webgateway_cache_max_bytes`；
  - 连接：`webgateway_connections_active`、`webgateway_requests_in_flight`；
  - 日志：`webgateway_access_log_dropped_total`、`webgateway_access_log_failed_batches_total`、
    `webgateway_access_log_queue_available`；
  - 配置：`webgateway_config_sync_success_total`、`webgateway_config_sync_failures_total`、
    `webgateway_route_hosts`、`webgateway_certificate_hosts`；
  - 健康检查：`webgateway_health_check_success_total`、`webgateway_health_check_failure_total`、
    `webgateway_backends_unhealthy`；
  - 限流：`webgateway_rate_limited_total`、`webgateway_rate_limit_tracked_ips`；
  - 构建：`webgateway_info{machine_id,node,version}`。
- dashboard（控制面）：
  - `dashboard_up`、`dashboard_database_up`、`dashboard_uptime_seconds`；
  - `dashboard_websites_total`、`dashboard_certificates_total`、`dashboard_certificates_expiring_total`、
    `dashboard_gateways_online`、`dashboard_gateways_total`、`dashboard_users_total`；
  - `dashboard_stats_ready`、`dashboard_stats_lag_seconds`；
  - `dashboard_auth_failures_total`、`dashboard_auth_throttled_total`、`dashboard_api_errors_total`；
  - `dashboard_info{schema_version}`。

告警规则示例（阈值按业务调整）：

```yaml
groups:
  - name: webgateway
    rules:
      - alert: DashboardDown
        expr: up{job="webgateway-dashboard"} == 0
        for: 2m
      - alert: GatewayDown
        expr: up{job="webgateway-gateway"} == 0
        for: 1m
      - alert: DashboardDatabaseDown
        expr: dashboard_database_up == 0
        for: 2m
      - alert: AccessLogDropping
        expr: increase(webgateway_access_log_dropped_total[5m]) > 0
      - alert: ConfigSyncFailing
        expr: increase(webgateway_config_sync_failures_total[10m]) > 0
      - alert: BackendsUnhealthy
        expr: webgateway_backends_unhealthy > 0
        for: 5m
      - alert: RateLimitingActive
        expr: increase(webgateway_rate_limited_total[5m]) > 100
      - alert: CacheHitRatioLow
        expr: sum(rate(webgateway_cache_hits_total[10m])) / clamp_min(sum(rate(webgateway_cache_hits_total[10m])) + sum(rate(webgateway_cache_misses_total[10m])), 0.001) < 0.5
        for: 15m
      - alert: StatsLagging
        expr: dashboard_stats_lag_seconds > 300
        for: 5m
      - alert: CertificateExpiring
        expr: dashboard_certificates_expiring_total > 0
        for: 1h
      - alert: LoginFailures
        expr: increase(dashboard_auth_failures_total[10m]) > 20
```

注意：

- 指标端点没有认证，务必只绑定回环/内网；需要远程抓取时用代理或 SSH 隧道。
- 计数器是进程内计数，重启归零；告警请用 `rate`/`increase`，不要依赖绝对值。
- Alertmanager、日志聚合与 tracing 导出不在本仓库范围内。

## 12. 生产就绪状态（2026-11-20）

6 个批次（每批含测试、文档、提交与 CI）完成的生产阻断项：

| 领域 | 状态 | 分支 / 提交 | 验证 |
|---|---|---|---|
| 面板 Cookie 会话 / 安全头 / 错误脱敏 | ✅ | feat/dashboard-session-security（1d0ba33/005bfff） | E2E 登录、撤销、CSP/HSTS |
| 面板静态托管 / SPA / SIGTERM 30s drain | ✅ | feat/production-deploy（ac74a83） | `/`、资源缓存、SIGTERM exit 0 |
| 控制面密钥 AES-256-GCM 静态加密 | ✅ | feat/production-deploy（340d3c4/27b92f6） | 明文迁移、gateway 解密证书 |
| Prometheus 指标（dashboard/gateway） | ✅ | feat/observability（a54eb85） | 缓存/日志/健康/统计指标 |
| 每 IP 令牌桶限流 | ✅ | feat/rate-limit（ad4196f） | 200/200/429 + Retry-After |
| 站点级上游健康检查 / 失败摘除 | ✅ | feat/health-checks（9fb72f1） | 死后端自动摘除、指标 |
| HTTP/3 流式响应 / 停机 drain | ✅ | feat/h3-streaming（30ac6b0） | 64 KiB 流式、慢请求 drain |
| 缓存 Vary / HEAD / 304 | ✅ | feat/cache-vary（61d6c0b） | 变体隔离、HEAD/304 |
| 统计 distinct IP / 保留期 / 面板时区 | ✅ | feat/stats-accuracy（4a0450b） | minute/daily distinct、tz offset |
| 仓库主线整理（B-21） | ✅ | master de1da22（原 v2 + 31 个 feature 分支合并）；v1 为旧原型 | 分支已删除，默认分支 master |
| mnt TOTP CLI / Docker 打包 | ✅ | mnt + Dockerfile/docker-compose | socket 0600、mnt code 登录 200、compose YAML |
| 面板 UX 批量改造 | ✅ | 本轮 master（00b1955 之后） | Toast/提示、分组菜单、DNS 页、结构化站点表单、日志详情、自动刷新、自定义 Select |
| 证书申请代理 | ✅ | 本轮 master | 设置页保存（加密）、ACME 客户端经代理；DNS API 直连 |
| 网关默认 80/443 | ✅ | 本轮 master | systemd AmbientCapabilities、Docker 80/443 + 非 root 端口 sysctl |

**最终验证**（2026-11-20）：

- 测试：gateway 53、dashboard 26、shared 19 + 3 集成；`cargo fmt` / `clippy -D warnings` 通过；
  前端 format/lint/type-check/build 通过。
- 分支整理：远端只保留 master（de1da22，默认分支）与 v1（7ee8c9a）；31 个历史 feature 分支已合并删除。
- 历史分支 head CI 均绿：a5aee76（run 44716905077）、30ac6b0（run 44718152074）、
  88b0191（run 44718646839）、b22c12b（run 44720149107）。
- 叠加功能 smoke：加密私钥证书 HTTPS 200；h3 64 KiB 响应（响应上限 1 KiB）200 + 完整字节；
  缓存 MISS→HIT；死后端被健康检查摘除；面板静态托管 200（面板 CSP）；两端 `/metrics` 正常；
  3s 慢 h3 请求在 SIGTERM 后 200，gateway exit 0；dashboard SIGTERM exit 0；端口全部释放。

**结论**：代码层面已具备生产部署条件。上线前按第 8 节检查表执行，使用单 dashboard 实例、TLS 反向代理，
并为 dashboard 与所有 gateway 配置同一 `SECRET_ENCRYPTION_KEY`。B-21 已完成（master/v1 重整，
feature 分支删除）。剩余为第 9 节列出的增强项与可选外部接入（集中告警、CI DB E2E）。
## 13. Docker / Docker Compose 部署

仓库根目录提供多阶段 `Dockerfile`、`docker-compose.yml`、`docker/entrypoint.sh` 和
`docker.env.example`。镜像内包含 dashboard、gateway、`webgateway-mnt` 以及已构建的面板静态文件；
运行时基于 Debian bookworm-slim，非 root 用户 UID/GID 为 10001。

### 13.1 快速开始

```bash
cp docker.env.example .env
# 编辑 .env：至少填写 DATABASE_URL、SECRET_ENCRYPTION_KEY（openssl rand -hex 32）
docker compose up -d --build
docker compose ps
curl -fsS http://127.0.0.1:3000/health
```

面板监听 `3000`（映射到宿主 3000），网关默认监听容器内 `80` 和 `443`（映射到宿主 80/443；compose 通过
`net.ipv4.ip_unprivileged_port_start=0` 允许非 root 用户绑定）。
`SECRET_ENCRYPTION_KEY` 在 compose 中为必填；dashboard 与所有 gateway 必须使用同一个值。
数据库使用外部 PostgreSQL（compose 不内置 Postgres）；gateway 启动前 dashboard 必须完成迁移。

### 13.2 Compose 服务

| 服务 | 作用 | 说明 |
|---|---|---|
| `mnt-init` | 修正共享卷权限 | 以 root 运行一次，把 `/run/webgateway` chown 给 UID 10001 |
| `dashboard` | 主控 + 面板 | `3000`，健康检查 `/health`；首次启动迁移 schema |
| `gateway` | 边缘节点 | 默认 `80`/`443`，依赖 dashboard 健康 |
| `mnt` | 本地运维 CLI | profile `tools`，默认不启动，按需 `run --rm` |

镜像通过 `docker compose build` 构建；dashboard/gateway/mnt 共用同一个镜像，仅 command 不同。
生产部署时保持宿主 80/443；如需在前面放 TLS 反向代理/负载均衡，可调整 `GATEWAY_PORTS` 与
映射端口。HTTP/3 需要额外设置 `GATEWAY_HTTP3_PORTS`（例如 443）并放行 UDP 443。

### 13.3 本地取 TOTP

```bash
# 当前 6 位验证码
docker compose --profile tools run --rm mnt admin-totp
# 首次绑定用：secret + otpauth URI
docker compose --profile tools run --rm mnt --secret admin-totp
```

## 14. mnt 本地运维 CLI

`webgateway-mnt` 通过 Unix socket（默认 `/run/webgateway/mnt.sock`，可用 `MNT_SOCKET` 覆盖；systemd 用
`RuntimeDirectory=webgateway` 创建该目录，避免 `PrivateTmp` 隐藏 socket）
向 dashboard 请求管理员 TOTP。socket 由 dashboard 以 `0600` 创建，
**仅限本机/同容器访问**；不经过网络，也不会出现在 HTTP API 中。

```bash
# 取得当前验证码（登录面板时输入）
webgateway-mnt admin-totp

# 首次绑定：输出 secret 与 otpauth URI（可手动加入验证器）
webgateway-mnt --secret admin-totp

# 指定 socket（容器/自定义部署）
webgateway-mnt --socket /run/webgateway/mnt.sock admin-totp
```

输出示例：

```text
User: admin
TOTP: 123456
```

宿主机 systemd 部署时，把 release 的 `webgateway-mnt` 安装到 `/usr/local/bin/`（可再建 `mnt` 软链），
并以 root 运行；socket 默认路径与 dashboard 一致。Docker 部署直接使用第 13.3 节的 compose 命令。

证书申请代理：面板“设置 → 证书申请代理”保存一个可选的 HTTP(S) 代理 URL（加密存储），
仅用于 ACME CA 请求；Tencent DNSPod API 仍直连。留空即直连。代理 URL 变更后下一次签发生效。

安全说明：TOTP 是管理员登录因子，所以 mnt 故意只提供本地 socket；能读该 socket/secret 的账号等同于管理员。
不要把 socket 放到共享目录或挂载给不可信容器。
