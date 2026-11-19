# WebGateway v2 网关（数据面）

> 网关只读取数据库、不执行任何 DDL；主控初始化完成之前网关会等待。
> 访问日志/统计、缓存、IP-UA 拦截、WebSocket/HTTP3 尚未实现，见文末。

## 1. 启动与生命周期

1. `wait_for_master_schema`：轮询 `schema_state`，主控未初始化时等待（默认 300s，0 = 无限）。
2. 节点身份：`machine-id` crate；country、city、name、version 来自配置/环境。
3. 注册 `gateways`：status = online、started_at、last_seen_at；按 machine_id 冲突更新。
4. 首次全量同步 websites + certificates；之后由 **LISTEN/NOTIFY** 驱动：
   - 订阅 `websites_updater` / `certificates_updater`（INSERT/UPDATE/DELETE；DELETE 由迁移 0002 提供）；
   - 收到通知立即全量同步，多次通知自动合并；
   - 监听连接每次（重）连后发送 Connected：主循环比较水位
     `(COUNT(*), MAX(updated_at))`（websites / certificates 各一组），相同则跳过，不同才全量同步；
   - **不再每 10s 全量轮询**。
5. 监听端口 = `[server].ports` ∪ 所有 `websites.ports`；同步后自动增删监听。
6. 心跳每 10s 更新 `last_seen_at`；进程崩溃时心跳停止，`gateways_online` 视图 30s 后判定离线。
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
| `GATEWAY_PORTS` | 逗号分隔的监听端口 |
| `GATEWAY_BIND` | 监听地址 |

## 3. 路由

- Host 归一化：小写、去端口、去结尾点；支持 IPv6 字面量。
- 精确匹配优先，然后通配符 `*.example.com`（单 label，最长后缀优先）。
- `websites.backends` 是 JSONB 数组，每项可带匹配规则：

  ```json
  {
    "url": "http://api-backend:9000",
    "match_path": "/api",
    "match": {
      "path": "/api/*",
      "user_agent": "*Mobile*",
      "query": { "version": "v2" },
      "method": ["POST"]
    }
  }
  ```

  - `match_path`（旧字段）：路径前缀；`match.path`：支持 `*` / `?` 的 glob（区分大小写）；
  - `match.user_agent`：大小写不敏感 glob；
  - `match.query`：所有键都要匹配，空值表示只要求存在，请求值按 URL 解码；
  - `match.method`：忽略大小写，空数组表示不限制；
  - 所有配置条件 AND；命中后端轮询；没有任何后端命中时回退到所有后端。
- 上游只支持 `http://`（含 gRPC h2c）；`https://` 后端跳过并告警（上游 TLS 未实现）。

## 4. 请求头（站点级配置）

转发到上游的请求头由 `websites.config.headers` 决定：

````json
{
  "headers": {
    "set": {
      "X-Real-IP": "$remote_addr",
      "X-Forwarded-For": "$remote_addr",
      "X-Forwarded-Proto": "$scheme",
      "X-Forwarded-Host": "$host",
      "X-Custom": "node=$host"
    },
    "remove": ["X-Internal"]
  }
}
````

- 占位符：`$remote_addr`、`$scheme`、`$host`（原始 Host，含端口）。
- **未配置 `headers`**：使用默认四个头（X-Real-IP / X-Forwarded-For / X-Forwarded-Proto / X-Forwarded-Host）。
- **配置了 `headers`**：只使用配置里的 set/remove，不再自动补默认头。
- 逐跳头（Connection、Transfer-Encoding、Upgrade 等）始终剔除；Host 默认保留原值，可在 `set` 中覆盖。
- 非法头名/值会告警或跳过；配置在同步时解析。

### 4.1 客户端 IP 与 PROXY protocol

- 连接进入时先探测/消费 PROXY header（v1 文本 `PROXY TCP4 ...`、v2 二进制签名），再到 TLS/HTTP；
  每站点通过 `websites.config.get_request_ip` 决定是否信任：

  ```json
  { "get_request_ip": { "type": "proxyProtocol" } }
  ```

  支持的模式（与 v1 命名兼容）：

  | type | 说明 |
  |---|---|
  | `raw`（默认） | 使用 TCP 对端地址 |
  | `proxyProtocol` | 使用 PROXY v1/v2 的源地址；没有头则回退对端 |
  | `xForwardedFor` | `data` 为索引（0 = 最左/原始客户端，越界取最后一个） |
  | `xRealIP` | 使用 `X-Real-IP` 头 |

- CDN 场景：CDN 用 PROXY protocol 连接时站点启用 `proxyProtocol`；只加头时用 `xForwardedFor` / `xRealIP`。
- 未启用 `proxyProtocol` 的站点即使连接带了 PROXY header 也会被消费，但仍使用对端地址（不信任）。
- 实测：无头 → 127.0.0.1；v1 头 → 198.51.100.7；v2 头 → 203.0.113.9，并写入上游头。

## 5. 代理与响应

- 上游传输：普通请求 HTTP/1.1（60s 超时）；`content-type: application/grpc*` 的请求走独立的 HTTP/2-only 客户端（h2c），
  保留 `TE: trailers` 并透传 trailers，不套 60s 超时；超时 504、上游错误 502、未匹配站点 404。
- 响应头：`Server: WebGateway`、`X-WebGateway-Node`。
- `X-WebGateway-Node` 格式：`<location>-<16 hex>`，例如 `cn-shanghai-18411588dcb04048`。
  `location` = country-city 的 ASCII 小写形式；hash = FNV-1a64(`location + machine_id`)。

## 6. TLS

- 证书来自 `certificates`（`fullchain` + `private_key` PEM），过期证书不加载。
- SNI 精确 → 通配符 → 默认自签证书（rcgen，CN localhost）。
- ALPN：`h2`、`http/1.1`；E2E 实测 HTTPS 协商到 HTTP/2。

## 7. 已验证（beta 库）

- NOTIFY：SQL UPDATE 增加 Host 后 1s 内生效；DELETE 后 1s 内返回 404；迁移 0002 已由主控应用（schema version 2）。
- 站点 headers：回显上游确认 `x-custom: from-gateway.test`、`x-forwarded-for: 127.0.0.1`；
  未配置的 X-Real-IP / X-Forwarded-Proto 不再出现。
- 节点 header：`cn-shanghai-18411588dcb04048`。
- PROXY protocol：无头 / v1 / v2 三种连接分别解析出 127.0.0.1 / 198.51.100.7 / 203.0.113.9，并写入上游头。
- 路由：POST `/api/x?version=v2` + curl UA → upstream-one；version/method/UA 任一不匹配 → upstream-two；`/web` → upstream-two。
- HTTP 200、HTTPS HTTP/2 200、未知 Host 404、心跳更新、SIGTERM offline。
- 网关单测 23 个；shared 迁移/单测、fmt、clippy 全通过。

## 8. 尚未实现（下一步）

- 访问日志写入（四张周分区表）、秒/分/日聚合与保留期。
- 网关缓存（主控下发：大小、文件类型、ETag、TTL 等，细节待定）。
- IP / User-Agent 拦截（优先级低）。
- 上游传输扩展：
  - **WebSocket**：服务端 `serve_connection_with_upgrades` + 原始上游握手 + upgrade 双向桥接；
  - **HTTP/3**：QUIC（quinn）UDP 监听与 0-RTT、连接迁移；
  - **特殊 RFC**：CONNECT 等，按需协议分发层。
- 更复杂的路由（header / 正则）、上游 TLS、后端权重/主备。

## 9. 运行

````bash
# 配置：复制 config.example.toml 为 config.toml，或用环境变量
DATABASE_URL='postgres://...' GATEWAY_PORTS=8080,8443 cargo run -p gateway
````
