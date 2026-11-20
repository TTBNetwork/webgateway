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
HTTP/TCP、flush 访问日志并标记 offline（HTTP/3 在途请求尚未 drain）。

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

## 9. 已知生产限制（上线前必须评估）

| 级别 | 限制 | 说明 |
|---|---|---|
| P0 | 分支未合并 | 25 个堆叠分支、无主线集成（B-21），发布流程需先确定基线 |
| P1 | 密钥依赖环境变量 | 未设置 `SECRET_ENCRYPTION_KEY` 时仍明文入库；密钥丢失不可恢复；无自动重加密（B-31/P-13） |
| P1 | 告警需自建 | dashboard/gateway 已导出 Prometheus 指标，但未内置 Alertmanager/追踪；需按第 11 节接入（B-25 部分修复） |
| P1 | HTTP/3 全缓冲 | 大响应占内存、无缓存、停机不 drain（B-04/P-01） |
| P1 | 缓存正确性 | 无 `Vary`/single-flight/HEAD/If-None-Match: *（B-09） |
| P1 | 健康状态为单节点 | 站点级主动探测 + 被动失败标记已实现；无跨节点健康状态、无熔断/半开（P-10/U-17 部分修复） |
| P1 | 无 WAF/CC 规则 | 已有每客户端 IP 令牌桶限流（429 + Retry-After）；无 WAF/CC 规则与全局配额（U-07 部分修复） |
| P2 | 统计偏差 | `total_ips` 跨秒重复、保留期硬编码、面板按 UTC 日（B-08/B-14） |
| P2 | 登录限流单实例 | 进程内计数，多实例不共享（B-29） |
| P2 | CI 无 DB E2E | shared 集成测试在 CI 中跳过；h3/WS/gRPC 仅本地验证（U-31/B-22） |
| P2 | 版本与分页 | 面板版本/schema 硬编码、列表无服务端分页（B-17/B-18） |
| P3 | 依赖审计 | 无 cargo audit / 前端依赖审计（B-23） |

## 10. 运维手册（当前能力）

- 健康检查：`GET /health`（数据库 + schema 版本）。节点在线状态看 `gateways.status` 与
  `last_seen_at`（心跳 10s，面板按 30s 判定在线）。
- 日志：stdout/stderr 交给 journald；`RUST_LOG` 控制级别；访问日志写库，不落盘。
- 备份：`pg_dump -Fc` 全库；恢复后先启动 dashboard 校验 schema。
- 证书续期：dashboard 调度器在到期前 30 天自动续期（`auto_renew` 行）；手写上传证书不自动续期。
- 缓存：任意配置/证书 NOTIFY 会清空节点缓存，属预期行为。
- 升级：先 dashboard 后 gateway；gateway 滚动重启会 drain 10s，h3 连接直接断开。
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

