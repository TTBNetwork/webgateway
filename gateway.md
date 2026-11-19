# WebGateway v2 网关（数据面）

> 网关只读取数据库、不执行任何 DDL；主控初始化完成之前网关会等待。
> 访问日志/统计、缓存、IP-UA 拦截尚未实现，见文末。

## 1. 启动与生命周期

1. `shared::database::wait_for_master_schema`：轮询 `schema_state`，主控未初始化时等待（默认 300s，0 = 无限）。
2. 节点身份：`machine-id` crate 生成 machine id；配置/环境提供 country、city、name、version。
3. 注册 `gateways`：status = online、started_at、last_seen_at；冲突按 machine_id 更新。
4. 首次全量同步 websites + certificates，然后每 10s 同步一次。
5. 监听端口 = `[server].ports` ∪ 所有 `websites.ports`；端口集合随同步自动增删。
6. 心跳每 10s 更新 `last_seen_at`；崩溃时心跳停止，`gateways_online` 视图 30s 后判定离线。
7. SIGTERM / Ctrl-C：停止监听、标记 offline + stopped_at、关闭连接池。

## 2. 配置

````toml
[database]
url = "postgres://user:password@host:5432/webgateway_beta_v2"
max_connections = 10
connect_timeout_secs = 10
wait_timeout_secs = 300

[node]
name = "edge-1"
country = "CN"
city = "Shanghai"
# address = "203.0.113.10"

[server]
bind = "0.0.0.0"
ports = [80, 443]
````

| 环境变量 | 覆盖项 |
|---|---|
| `DATABASE_URL` | `database.url` |
| `DATABASE_MAX_CONNECTIONS` / `DATABASE_CONNECT_TIMEOUT` / `DATABASE_WAIT_TIMEOUT` | 同名数据库配置 |
| `GATEWAY_NODE_NAME` / `GATEWAY_NODE_COUNTRY` / `GATEWAY_NODE_CITY` / `GATEWAY_NODE_ADDRESS` | 节点信息 |
| `GATEWAY_PORTS` | 逗号分隔的监听端口，例如 `8080,8443` |
| `GATEWAY_BIND` | 监听地址，默认 0.0.0.0 |

## 3. 路由

- Host 归一化：小写、去端口、去结尾点；支持 IPv6 字面量。
- 精确匹配优先，然后通配符 `*.example.com`（只匹配单 label，最长后缀优先）。
- `websites.backends` 为 JSONB 数组：`{ "url": "http://127.0.0.1:9001", "match_path": "/api" }`。
  `match_path` 为前缀匹配，命中的后端按轮询选择；没有命中时在所有后端间轮询。
- 当前只支持 `http://` 上游；`https://` 后端会跳过并告警（上游 TLS 未实现）。
- 站点 `ports` 决定额外监听端口，不参与 Host 路由。

## 4. 代理行为

- 请求头：保留原 Host；覆盖 `X-Real-IP`、`X-Forwarded-For`；补 `X-Forwarded-Proto`、
  `X-Forwarded-Host`；剔除逐跳头（Connection、Transfer-Encoding、Upgrade 等）。
- 上游固定 HTTP/1.1；连接复用由 hyper-util 连接池负责。
- 上游响应 60s 超时；超时 504，上游错误 502，无匹配站点 404。
- 响应头：`Server: WebGateway`、`X-WebGateway-Node: <country-city>`（非 ASCII 会过滤，空则用 machine id）。

## 5. TLS

- 证书来自 `certificates`：`fullchain` + `private_key`（PEM），过期证书不加载。
- SNI 精确匹配 → 通配符匹配 → 默认自签证书（启动时用 rcgen 生成，CN localhost）。
- ALPN：`h2`、`http/1.1`；E2E 实测 HTTPS 协商到 HTTP/2。
- 无 SNI 或未知域名也能握手（默认证书），请求路由仍按 Host 判断。

## 6. 已验证（beta 库）

- HTTP `http://127.0.0.1:8080/`（Host: gateway.test）→ 200，响应带节点 header。
- HTTPS `https://gateway.test:8443/`（-k，默认证书）→ HTTP/2 200。
- 未知 Host → 404。
- `gateways` 行 online，10s 心跳更新 last_seen_at；SIGTERM 后 status = offline、stopped_at 写入、
  `gateways_online` 计数 0。
- 单元测试 9 个（Host 归一化、通配符路由、轮询、IPv6 上游、PEM 解析、节点 header 等）。

## 7. 尚未实现（后续）

- 访问日志写入（四张周分区表）、秒/分/日聚合与保留期清理。
- 网关缓存：主控下发的缓存大小、文件类型、ETag、TTL 等（细节待定）。
- IP / User-Agent 拦截（优先级低）。
- WebSocket upgrade、上游 TLS、PROXY protocol、站点 `config.get_request_ip`、后端权重/主备。
- 配置增量同步：当前每 10s 全量同步，NOTIFY 快速触发待接入。

## 8. 运行

````bash
# 配置：复制 config.example.toml 为 config.toml，或用环境变量
DATABASE_URL='postgres://...' GATEWAY_PORTS=8080,8443 cargo run -p gateway
````
