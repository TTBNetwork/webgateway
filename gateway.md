# WebGateway v2 网关（数据面）

> 网关只读取数据库、不执行任何 DDL；主控初始化完成之前网关会等待。
> IP-UA 拦截尚未实现，见文末。

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
# UDP；为空不启动 HTTP/3
http3_ports = [443]

[upstream_tls]
# 额外的 PEM CA，用于 https 后端；webpki 根证书始终受信
# ca_file = "/etc/webgateway/upstream-ca.pem"
````

| 环境变量 | 覆盖项 |
|---|---|
| `DATABASE_URL` | `database.url` |
| `DATABASE_MAX_CONNECTIONS` / `DATABASE_CONNECT_TIMEOUT` / `DATABASE_WAIT_TIMEOUT` | 同名数据库配置 |
| `GATEWAY_NODE_NAME` / `GATEWAY_NODE_COUNTRY` / `GATEWAY_NODE_CITY` / `GATEWAY_NODE_ADDRESS` | 节点信息 |
| `GATEWAY_PORTS` | 逗号分隔的 TCP 监听端口 |
| `GATEWAY_HTTP3_PORTS` | 逗号分隔的 UDP 端口（HTTP/3） |
| `GATEWAY_UPSTREAM_CA_FILE` | https 后端的额外 CA PEM |
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
    },
    "weight": 3,
    "priority": 0
  }
  ```

  - `match_path`（旧字段）：路径前缀；`match.path`：支持 `*` / `?` 的 glob（区分大小写）；
  - `match.user_agent`：大小写不敏感 glob；
  - `match.query`：所有键都要匹配，空值表示只要求存在，请求值按 URL 解码；
  - `match.method`：忽略大小写，空数组表示不限制；
  - `weight`（默认 1）：同一优先级组内按权重轮询；`priority`（默认 0）：数值越小越优先，
    数值更大的后端是备用组，只有低优先级组没有匹配后端时才参与；
  - 所有配置条件 AND；命中后端按权重轮询；没有任何后端命中时回退到所有后端。
  - 当前**不做失败自动切换**（备用组不是健康检查故障转移）。
- 上游支持 `http://` 与 `https://`；https 使用 webpki 根 + 可选 `[upstream_tls].ca_file`，gRPC 与
  WebSocket 升级（wss）同样支持 TLS，WebSocket 还会拼接后端 base path。

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

### 5.1 访问日志

- 每个请求写 `access_request_logs` + `access_response_logs`；请求/响应字节数 > 0 时再写
  `access_request_size_logs` / `access_response_size_logs`（按秒，`ON CONFLICT` 累加）。
- 后台 writer 每 500ms（或满 256 条）批量写一个事务；所有时间取 PostgreSQL `now()`，与主控时钟一致。
- 字段：Host、method、path、headers（JSON 对象）、HTTP 版本、客户端 IP（`get_request_ip` 解析后的）、
  `website_id`、status、响应字节（Content-Length）。
- 网关自己生成的 404/502/504 也会记录（body_length 为固定错误文本长度）。
- 限制：正文长度取 Content-Length，chunked/未知请求为 0、响应为 NULL；writer 关闭时直接 abort，
  最后一批可能丢。统计聚合由主控 stats 任务负责（见 database.md 5.4）；原始日志保留 7 天。

### 5.2 WebSocket 与 CONNECT

- **WebSocket**：请求头 `Upgrade: websocket` 时走升级通路：
  - 服务端使用 `serve_connection_with_upgrades`；
  - 到上游做原始 HTTP/1.1 握手，客户端连接启用 `.with_upgrades()`；
  - 上游返回 101 后双向桥接；非 101 按普通响应回传；
  - 访问日志记录 101。
- **CONNECT**：仅允许 authority 的 host 命中已配置站点（否则 403），连接 `host:port` 后返回 200
  并桥接原始 TCP；不是 TLS MITM，只做透传。访问日志记录 CONNECT + 200。
- 上游支持 `http://` 与 `https://`：https 后端先用同一 TLS 配置（webpki 根 + 可选 ca_file）握手，
  ALPN 固定 `http/1.1`；后端 URL 的 base path 会拼到客户端 path 前（`/mc` + `/socket.io/...`）。
- 限制：HTTP/2 extended CONNECT / WebSocket over h2 未实现；升级连接不计字节数（body_length 0）；
  无 mTLS / SNI 覆盖。
- 已用 MCSManager v10.22.1（daemon 4.19.3）实测：明文 ws、wss（测试 CA）、带 base path 的 wss `/mc`
  三种场景均通过；升级 101、Socket.IO 握手、daemon key 鉴权、`info/overview` 与 engine.io ping/pong 正常。
  详见 `e2e-realtime.md`。

### 5.3 HTTP/3（QUIC）

- 需要显式开启：`[server].http3_ports` 或 `GATEWAY_HTTP3_PORTS`（UDP）；为空不启动。
- QUIC 使用 TLS 1.3 + ALPN `h3`，证书复用 TLS 的 SNI 存储（含默认自签证书）。
- 请求体完整缓冲后，通过内部 HTTP/1.1 buffered client 转发上游；响应体也完整缓冲后发送。
- 访问日志记录 `HTTP/3.0`；`get_request_ip` 的 raw/XFF 模式可用，PROXY protocol 不适用。
- 限制：无流式请求/响应（大 body 占内存）；每个请求独立 spawn、可并发处理；
  WebSocket / gRPC over h3 未实现；未调 0-RTT / 连接迁移。

### 5.4 缓存

站点通过 `websites.config.cache` 开启：

```json
{
  "cache": {
    "enabled": true,
    "ttl": 60,
    "content_types": ["text/css", "image/"],
    "max_entry_size": 1048576
  }
}
```

- 仅缓存 GET + 200 + 无 `Set-Cookie` / `Vary` / `Cache-Control: no-store|private|no-cache` 的响应；
  请求带 `Authorization` 或 `Cookie` 时跳过缓存。
- `content_types` 为空表示所有类型；匹配忽略大小写，`image/` 这类以 `/` 结尾的按前缀匹配。
- TTL 取站点 `ttl` 与上游 `s-maxage` / `max-age` 的较小值。
- 全局内存上限默认 **16 MiB**（`[server].cache_max_size_bytes` / `GATEWAY_CACHE_MAX_SIZE`），LRU 淘汰；
  单条上限 `max_entry_size`；重启后缓存清空。
- 命中：`X-WebGateway-Cache: HIT` + `Age`；未命中：`MISS`；`If-None-Match` 命中返回 304；
  上游无 ETag 时生成弱 ETag。
- 配置同步（NOTIFY）时清空缓存。
- 限制：不缓存 HEAD/POST、不支持 `stale-while-revalidate`、不区分 Vary 维度、无持久化/多级缓存。

## 6. TLS

- 证书来自 `certificates`（`fullchain` + `private_key` PEM），过期证书不加载。
- SNI 精确 → 通配符 → 默认自签证书（rcgen，CN localhost）。
- ALPN：`h2`、`http/1.1`；E2E 实测 HTTPS 协商到 HTTP/2。
- HTTP/3 使用同一证书存储，ALPN `h3`、TLS 1.3（QUIC）。

## 7. 已验证（beta 库）

- NOTIFY：SQL UPDATE 增加 Host 后 1s 内生效；DELETE 后 1s 内返回 404；迁移 0002 已由主控应用（当前 schema version 3）。
- 站点 headers：回显上游确认 `x-custom: from-gateway.test`、`x-forwarded-for: 127.0.0.1`；
  未配置的 X-Real-IP / X-Forwarded-Proto 不再出现。
- 节点 header：`cn-shanghai-18411588dcb04048`。
- PROXY protocol：无头 / v1 / v2 三种连接分别解析出 127.0.0.1 / 198.51.100.7 / 203.0.113.9，并写入上游头。
- 路由：POST `/api/x?version=v2` + curl UA → upstream-one；version/method/UA 任一不匹配 → upstream-two；`/web` → upstream-two。
- 访问日志：GET/POST/404 分别产生 request/response 行；POST 5 字节、响应 12/9 字节，size 表按秒记录。
- WebSocket：Python WS echo 经网关回显 `hello-websocket`（101）。
- CONNECT：`CONNECT localhost:9500` 返回 200，`ping-through-connect` 原样回显。
- HTTP/3：aioquic 客户端 GET `/h3` → 200，body `http3-upstream-ok`；访问日志 `HTTP/3.0`。
- 统计：3 条合成日志 → 秒级 30/150 字节；旧秒归并分钟 70/700；旧分归并日 90/900。
- 缓存：MISS count=1 → HIT count=1 → 304 → 配置更新后 MISS count=2。
- 上游 TLS：本地 CA + `https://localhost:9443` 后端返回 `upstream-https-ok`。
- 权重/优先级：40 次请求按 3:1 分到 30/10，备用组 0 次。
- HTTP/3 并发：同一 QUIC 连接 5 个请求全部 200，按权重 4/1 分流。
- gRPC（Nezha Dashboard v2.6.2 + agent v2.4.0）：h2c 与 TLS/ALPN h2 两种入口均注册成功；
  `ReportSystemInfo2`、`ReportSystemState`、`ReportGeoIP` 返回 200，`RequestTask` 双向流保持打开，
  Dashboard `online_sessions.ongoing = 1` 直到 Agent 停止。Go gRPC 客户端未报 trailer 错误。
- WebSocket（MCSManager daemon 4.19.3）：升级 101、Socket.IO 鉴权、`info/overview`、长连接 ping/pong 通过。
- wss 上游（MCS daemon 开 SSL + 测试 CA）：经网关导入 `info/overview` 成功；后端 base path `/mc` 生效。
- HTTP 200、HTTPS HTTP/2 200、未知 Host 404、心跳更新、SIGTERM offline。
- 网关单测 33 个；shared 迁移/单测、fmt、clippy 全通过。

## 8. 尚未实现（下一步）

- 缓存增强：持久化/多级缓存、stale-while-revalidate、Vary 维度、命中率指标。
- IP / User-Agent 拦截（优先级低）。
- 上游传输扩展：
  - HTTP/3 流式请求/响应、0-RTT / 连接迁移调优；
  - HTTP/2 extended CONNECT / WebSocket over h2；
  - 其余特殊 RFC 按需补充。
- 更复杂的路由（header / 正则）、失败自动切换/健康检查、上游客户端证书。

## 9. 运行

````bash
# 配置：复制 config.example.toml 为 config.toml，或用环境变量
DATABASE_URL='postgres://...' GATEWAY_PORTS=8080,8443 GATEWAY_HTTP3_PORTS=8443 cargo run -p gateway
````
