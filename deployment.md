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
| `DASHBOARD_API_TOKENS` | 推荐 | 服务令牌，逗号分隔；轮换期间可同时配置新旧两个 |
| `DASHBOARD_API_TOKEN` | 兼容 | 单个服务令牌（已弃用，仍会合并到列表） |
| `DASHBOARD_TOKEN_EXPIRES` | 否 | 面板会话秒数，默认 604800，范围 300..2592000 |
| `DASHBOARD_ADMIN_TOTP_SECRET` | 首次 | 空库创建默认管理员时使用；不设则随机生成并只打印一次 |
| `DASHBOARD_ADMIN_USERNAME` | 否 | 默认管理员用户名，默认 `admin` |
| `DASHBOARD_JWT_SECRET` | 否 | 默认管理员的 JWT 密钥；不设则随机生成后入库 |
| `ACME_EMAIL` | 否 | 证书申请联系邮箱的兜底值 |
| `RUST_LOG` | 否 | 建议 `dashboard=info,shared=info` |

首次启动要点：

1. 空 `users` 表会创建默认管理员；TOTP secret 只在 WARN 日志中出现一次。
   生产建议先设置 `DASHBOARD_ADMIN_TOTP_SECRET`（base32）再启动，直接保存到密码管理器。
2. `DASHBOARD_API_TOKENS` 请生成高熵随机值（例如 `openssl rand -hex 32`），不要复用开发令牌。
3. 令牌与会话密钥目前**明文存储/环境变量**，见第 9 节限制。

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
| `RUST_LOG` | 建议 `gateway=info,shared=info` |

同一台机器跑多个 gateway 时必须使用不同的 `machine_id`（不同用户/容器），否则会争用同一节点记录与端口。

## 8. 首次上线检查表

1. 数据库建库、扩展权限、`pg_dump` 基线备份。
2. 部署 dashboard + `panel/`，配置 `DASHBOARD_STATIC_DIR`、`DASHBOARD_API_TOKENS`、TOTP secret。
3. 启动 dashboard，`curl -fsS https://panel.example.com/health` 返回 `status=ok`、schema 版本符合预期。
4. 浏览器登录面板（用户名 + TOTP），确认会话 Cookie 为 HttpOnly/Secure。
5. 面板中创建 DNS provider、证书（手动上传或 ACME）、站点与上游。
6. 启动 gateway，确认 `gateways.status = online` 且 10 秒内有心跳。
7. 域名解析指向 gateway，验证 HTTPS 证书、路由、访问日志与统计入库。
8. 打开站点缓存后验证 MISS→HIT；验证 413/431 等上限行为。
9. 做一次 `systemctl restart webgateway-dashboard` 和 `kill -TERM` gateway，确认无异常退出。

## 9. 已知生产限制（上线前必须评估）

| 级别 | 限制 | 说明 |
|---|---|---|
| P0 | 密钥明文 | TOTP/JWT secret、ACME account key、证书私钥、DNS 凭据目前明文存库（B-31/P-13） |
| P0 | 分支未合并 | 25 个堆叠分支、无主线集成（B-21），发布流程需先确定基线 |
| P1 | 无 metrics/告警 | 只有 `/health`；缓存命中、日志丢弃、上游错误不可观测（B-25） |
| P1 | HTTP/3 全缓冲 | 大响应占内存、无缓存、停机不 drain（B-04/P-01） |
| P1 | 缓存正确性 | 无 `Vary`/single-flight/HEAD/If-None-Match: *（B-09） |
| P1 | 无上游健康检查 | priority 是静态备用组，失败不会自动摘除（P-10/U-17） |
| P1 | 无限流/WAF | 无按 IP 的连接/请求限速（U-07） |
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
