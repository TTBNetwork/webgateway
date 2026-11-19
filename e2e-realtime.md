# 实时协议联调：Nezha gRPC 与 MCSManager WebSocket

> 目标：用真实软件验证网关的 gRPC 上游（HTTP/2 双向流 + trailers）与 WebSocket 升级/长连接，
> 不再只依赖自建的 echo 服务。
>
> 环境：beta PostgreSQL（测试开始时 schema v2，随后升级到 v3）、本地网关（18080 HTTP/h2c、18443 TLS/ALPN h2）、
> Nezha Dashboard v2.6.2 + nezha-agent v2.4.0、MCSManager v10.22.1（daemon 4.19.3）。

## 1. 拓扑

```
                    gRPC h2c / h2 over TLS                    h2c
nezha-agent  ───────────────►  gateway:18080/18443  ───────────────►  Nezha dashboard:8008
                             (host 127.0.0.1)                        (gRPC + Web 同端口)

                    WebSocket (Socket.IO v4)                    ws
MC 客户端    ───────────────►  gateway:18080        ───────────────►  MCS daemon:24444
                             (host 127.0.0.2)                        (/socket.io)
```

网关侧只需要两条站点配置（测试结束后已删除）：

```sql
-- gRPC：Nezha 的 :authority 为 127.0.0.1:18080，归一化后按 host 127.0.0.1 路由
INSERT INTO websites (id, hosts, backends)
VALUES ('e2e-nezha-grpc', ARRAY['127.0.0.1'],
        '[{"url":"http://127.0.0.1:8008"}]'::jsonb);

-- WebSocket：用 127.0.0.2 作为不同的 Host，避免与 gRPC 站点冲突
INSERT INTO websites (id, hosts, backends)
VALUES ('e2e-mcs-ws', ARRAY['127.0.0.2'],
        '[{"url":"http://127.0.0.1:24444"}]'::jsonb);
```

## 2. gRPC + Nezha

Nezha v2 的 Dashboard 在 Web 端口（默认 8008）上同时提供 h2c gRPC 与 HTTP/1.1：
`curl --http2-prior-knowledge` 与普通 HTTP/1.1 都能拿到 200。Agent 通过
`proto.NezhaService` 与服务端通信（`ReportSystemInfo2`、`ReportSystemState`、
`ReportGeoIP`、`RequestTask` 等），其中 `RequestTask` 是长连接双向流。

Agent 配置（经网关，明文 h2c）：

```yaml
debug: true
server: 127.0.0.1:18080
client_secret: <Nezha admin 用户的 agent_secret>
uuid: 33333333-3333-3333-3333-333333333333
report_delay: 1
disable_auto_update: true
disable_force_update: true
disable_command_execute: true
tls: false
```

TLS 变体：网关增加 18443 端口，Agent 改用 `server: 127.0.0.1:18443`、`tls: true`、
`insecure_tls: true`（使用网关默认自签证书）。

### 结果

| 场景 | 结果 | 说明 |
|---|---|---|
| gRPC h2c（18080） | ✅ | Agent 注册成功，Dashboard 自动创建 server 记录 |
| gRPC over TLS / ALPN h2（18443） | ✅ | 同上；会话 `ongoing = 1` 保持约 37s，直到 Agent 被停止 |
| 一元调用 | ✅ | `ReportSystemInfo2` / `ReportSystemState` / `ReportGeoIP` 均返回 200 |
| 服务端流 / 双向流 | ✅ | `RequestTask` 连接期间保持打开，会话未提前结束 |
| trailers | ✅（间接） | Go gRPC 客户端要求 `grpc-status` trailer；若被丢弃会立即报错，实际 Agent 无任何 RPC 错误 |

网关访问日志（实测）：

```
POST /proto.NezhaService/ReportSystemInfo2  HTTP/2.0  200
POST /proto.NezhaService/ReportSystemState  HTTP/2.0  200
POST /proto.NezhaService/ReportGeoIP        HTTP/2.0  200
POST /proto.NezhaService/RequestTask        HTTP/2.0  200
```

Nezha SQLite 中的在线会话（TLS 场景）：

```
servers:          id=3 name=improved-mallard uuid=33333333-...
online_sessions:  server_id=3 started_at=16:51:45 ended_at=NULL ongoing=1
（Agent 停止后）  ended_at=16:52:22 ongoing=0
```

### 说明 / 限制

- 访问日志的 `status` 是 HTTP 状态（200），不是 `grpc-status`；`body_length` 为
  0/NULL（gRPC 没有 Content-Length），已记录在 issue.md（B-19 相关）。
- gRPC 请求走 `state.grpc_client`（HTTP/2-only）；上游为 `http://` 时按 h2c 先验知识连接。
- Dashboard 把 gRPC 与 Web 放在同一端口，因此站点后端只需指向 8008。

## 3. WebSocket + MCSManager

MCSManager Daemon 使用 Socket.IO v4，路径 `/socket.io`；连接后必须在 6s 内发送
`auth` 事件（`{ uuid, data: <daemon key> }`，事件名 `auth`），然后才能调用
`info/overview` 等业务事件。Daemon 默认每 20s 发一次 engine.io ping。

探针脚本：`scripts/e2e/mcsmanager_ws_probe.mjs`（零依赖，使用 Node 全局 WebSocket）。

```bash
MCS_KEY='<daemon key>' HOLD_SECONDS=45 \
WS_URL='ws://127.0.0.2:18080/socket.io/?EIO=4&transport=websocket' \
node scripts/e2e/mcsmanager_ws_probe.mjs
```

### 结果

```
[mcs-probe] websocket open ws://127.0.0.2:18080/socket.io/?EIO=4&transport=websocket
[mcs-probe] engine.io open { pingInterval: 20000, pingTimeout: 10000 }
[mcs-probe] socket.io namespace connected
[mcs-probe] daemon auth accepted
[mcs-probe] info/overview { version: '4.19.3', instances: { running: 0, total: 0 }, platform: 'linux' }
[mcs-probe] holding the connection for 45s
[mcs-probe] engine.io ping received, sending pong
[mcs-probe] engine.io ping received, sending pong
[mcs-probe] PASS {... "ping":true ...}
```

| 场景 | 结果 | 说明 |
|---|---|---|
| 升级 101 | ✅ | 访问日志记录 `/socket.io/` HTTP/1.1 **101** |
| Socket.IO 握手 | ✅ | `0{"sid"...}` → `40` → `40{"sid"...}` |
| Daemon 鉴权 | ✅ | 真实 daemon key 校验通过 |
| 业务事件 | ✅ | `info/overview` 返回 daemon 版本 4.19.3 |
| 长连接 / 心跳 | ✅ | 45s 内收到 2 次 engine.io ping 并正确回 pong |
| 升级连接字节统计 | ❌ 已知限制 | WS 的 body_length 记为 0（B-19） |
| 上游 TLS（wss） | ❌ 已知缺陷 | 见下节 B-03 |

### 上游 TLS（wss）实测

把 MCS daemon 开启 SSL（`ssl: true` + 自签证书）后：

- 直连 `wss://127.0.0.1:24444/socket.io/...`：✅ 探针 PASS。
- 经网关（站点后端 `https://127.0.0.1:24444`）：❌ WS 升级失败。

原因即 issue.md 的 **B-03**：`gateway/src/upgrade.rs` 用裸 `TcpStream::connect` 直连后端，
不经过 `HttpsConnector`，所以 https 后端收到的是明文 HTTP；另外后端 base path 被忽略。
该问题在后续 B-03 修复中处理（需要验证证书/CA 配置）。

## 4. 复现步骤

1. 启动 Nezha Dashboard（`dashboard-linux-amd64 -c data/config.yaml`，监听 8008），
   从 SQLite `users.agent_secret` 取 agent 密钥。
2. 启动 MCSManager Daemon；密钥在 `daemon/data/Config/global.json` 的 `key`。
3. 按第 1 节插入两条站点配置。
4. 启动网关：
   `DATABASE_URL=... GATEWAY_PORTS=18080,18443 ./target/debug/gateway`。
5. gRPC：
   `NEZHA_AGENT=... NEZHA_DASHBOARD_DB=... NEZHA_SERVER=127.0.0.1:18080 NEZHA_SECRET=... bash scripts/e2e/nezha_grpc_probe.sh`
   （TLS：`NEZHA_SERVER=127.0.0.1:18443 NEZHA_TLS=true`）。
6. WebSocket：见第 3 节命令。
7. 清理：删除两条测试站点；停止 Agent / daemon / dashboard；删除 Nezha 与 MCS 的临时数据目录。

## 5. 结论

- gRPC（h2c、TLS、单向/服务端流/双向流、trailers）在真实 Nezha 监控上验证通过。
- WebSocket（升级、Socket.IO 握手、鉴权、业务事件、长连接心跳）在真实 MCSManager 上验证通过。
- 同时确认了 WS 上游 TLS（wss）与 base path 仍是已知缺陷（B-03），以及升级连接无字节统计（B-19）。
