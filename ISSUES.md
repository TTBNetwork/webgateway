# 已知与未知问题清单（审计报告）

> 生成方式：AI 静态审计，逐行核对代码得出，每条均附 `文件:行号` 证据。
> 审计范围：`gateway`、`dashboard/backend`、`dashboard/frontend`、`crates/*`、`assets/sqls/`。
> 基准提交：`64cbdad`（含当前未提交改动）。
> 术语：**P0** = 事故级（数据丢失 / 服务不可用 / 安全泄露）；**P1** = 严重；**P2** = 一般。

## 结论摘要

你提到的"数据库优化问题（多个程序抢一个表）"确实存在，而且不是单点问题，是一条完整的**雪崩链路**：

```text
面板每 5s 轮询 QPS
  → qps_per_second / qps_per_5s 视图强制全表扫描（谓词非 sargable）
  → access_request_logs 无分区/无清理，无限增长 → 扫描越来越慢
  → dashboard 连接池（默认 10）被慢查询占满 → 面板接口超时
  → PostgreSQL CPU 饱和 → 网关 1s 刷盘循环退化
  → sync() 超过 1s 后循环钳到 100µs 空转（无 sleep）→ 发起更多 DB 查询
  → 正反馈，直至数据库被拖垮
```

在此之上还叠加了**真正的数据丢失**：刷盘逻辑"先删内存、后写库"，一旦写入失败日志永久丢失；而批量 INSERT 未分块，高并发下会撞上 PostgreSQL 的 65535 参数上限必然失败——恰好发生在流量高峰。

另外发现三个**安全/可用性级**问题：上游连接池会把未读完响应的连接放回池中复用（响应串包/跨站点数据泄露）；SNI 预读无上限（单连接可申请 16MB，慢速攻击可耗尽内存）；**证书续签判定方向写反**（`expires_at < NOW() - 7 days`），证书会在到期后才开始续签，导致站点 HTTPS 中断。

权限模型缺失：任何一个已登录账号都等同管理员，可枚举全部账号并接管全部站点、证书与 DNS 凭据；`/auth/info` 还存在越权读取（IDOR）。

统计：**14 个 P0**、**16 个 P1**、**12 个 P2**。

---

## 修复状态（2026-10-02 第三轮更新）

> 下面的条目正文保持审计当时的原始描述（作为问题证据留存），
> **状态以本表为准**。实施细节见 [CHANGELOG.md](CHANGELOG.md)「第三轮」条目。

### 已修复

| 编号 | 修复摘要 | 验证方式 |
|------|---------|---------|
| P0-1 | 刷盘改「先写库成功、再删内存」，失败批次留内存重试；新增连续失败轮数与内存积压告警 | 真实 DB 驱动两轮刷盘：落库正确、二次刷盘不重复 |
| P0-1 补充 | `body_length` 批量 UPDATE 命中 0 行的静默丢失问题 | 真实 DB 验证 size 表与主表 `body_length` 均正确 |
| P0-2 | 批量 INSERT 按 ≤1000 行分块；批量 UPDATE 改 `FROM (VALUES ...)`（3→2 绑定/行） | 真实 DB 执行 `FROM (VALUES ...)` 形式通过 |
| P0-3 | QPS 改为直接过滤 `requested_at`（sargable）；视图去掉冗余 `ORDER BY` | `EXPLAIN` 确认 `Index Scan using idx_requested_at` |
| P0-4 | 刷盘下限 `.max(100µs)` → 最小 1 秒，超过 3s 告警 | 代码审查 + 参数化 |
| P0-5 | 仅响应体读到 EOF 才归还连接；删除恒真的 `write(&[])` 健康检查 | 真实网关 + 8MB 响应 + `curl --limit-rate` 强制中途断开：32/32 后续请求内容正确 |
| P0-6 | 连接池上限生效（默认 256）+ 取许可 5s 超时 | 代码审查；残留见下 |
| P0-7 | DDL 收敛单一迁移入口 + advisory lock；触发器单事务重建；恢复 10s 兜底同步 | 真实 DB 跑 `--migrate` 幂等通过；两进程不再并发 DDL |
| P0-8 | 站点/证书/监听器对账式同步 | 真实网关：改域名即时生效且旧域名 404；删站点后端口监听被回收并打日志 |
| P0-9 | 触发器改 `clock_timestamp()` | 真实 DB 读取 `pg_proc.prosrc` 确认；通知载荷时间戳单调递增 |
| P0-10 | `handshake_len` 上限 64KB + 预读 10s 超时 | 代码审查 + 协议层上限单点校验 |
| P0-11 | `retry: 0` + `timeout: 15000`，非 200 显式抛错 | `vue-tsc -b` 通过；组合式函数 9/9 运行时冒烟 |
| P0-12 | `useVisiblePolling` 隐藏暂停 / 恢复刷新 / 指数退避 | 同上（含 `visibilitychange` 监听器清理断言） |
| P0-13 | 续签方向改为 `expires_at < NOW() + '7 days'` + `expires_at` 索引 | 真实 DB 迁移含该索引与 SQL 语义核对 |
| P0-14 | 数据库侧 `FOR UPDATE SKIP LOCKED` 认领 + `Drop` 守卫释放 + 调度器句柄修正 | 真实 DB 验证认领/释放可用；`signing_started_at` 列已建 |
| P1-1 | 移除 release 下 `access_map` 空短路 + `(remote_addr, requested_at)` 索引 | 真实 DB 确认索引存在 |
| P1-4 | `exp` / `exp_at` 统一为秒 | 代码审查 |
| P1-5 | 路径拼接消除越界 panic 与 base path 丢失 | 代码审查 + 真实请求路径正确转发 |
| P1-6 | 通配符正则预编译 | 真实 TLS 握手：`*.txit.top` 正确匹配到对应证书 |
| P1-7 | 出锁后再做健康检查 | 代码审查 |
| P1-8 | 先 bind 成功再登记；失败明确报错 | 真实网关启动日志正常；端口回收日志可见 |
| P1-9 | `count` 限制 1..=300 | 代码审查 |
| P1-10 | 已由 P0-7 的启动模式拆分覆盖（服务进程不执行 DDL） | 真实启动为 `Serve` 模式且只做校验 |
| P1-11 | 防抖 + `AbortController` + 序号守卫 | `vue-tsc -b` 通过 |
| P1-13 | 访问地图数据不再丢弃；watcher 接通；`in_days` 透传 | `vue-tsc -b` 通过 |
| P1-15 | `/auth/info` 读他人需 admin | 代码审查 |
| P1-16 | `role` 列 + CHECK 约束 + `Authorizer`（admin/user/view） | **集成测试**：真实 DB + 真实 JWT，handler 级断言 `view` 调建站接口 403、`user` 200、`view`/`user` 调 `/auth/users` 403、`admin` 200；非法角色值被 CHECK 拒绝且 `Role::parse` 降级为 `view` |
| P2-1 | 视图去掉冗余 `ORDER BY` | 迁移通过 |
| P2-2 / P2-10 | metrics 失败不再静默停摆；bindTotp 绑定成功后停止轮询 | `vue-tsc -b` 通过 |

### 未修复（本轮明确不做，附原因）

| 编号 | 原因 / 后续 |
|------|------------|
| P0-3（分区/TTL） | 属 [CONVERSATION.md](CONVERSATION.md) 迁移方案范围，需停机迁移，另行排期。查询侧已走索引，但表仍会无限增长 |
| P0-6（空闲复用率） | 上游连接由 hyper `Connection` future 持有最长 120s，真正 keep-alive 归还需要在响应体读完后主动交还，属较大重构。**本次只解决了「无上限」与「许可提前释放」** |
| P1-2 上游 TLS / HTTP/2 | 未做 |
| P1-3 连接级硬超时截断 WebSocket/SSE | 未做 |
| P1-12 日志分页 N+1 | 未做 |
| P1-14 站点列表分页 | 未做 |
| P2-3～P2-9、P2-11、P2-12 | 未做（低优先级） |

### 本轮额外发现并修复（审计报告之外）

| 问题 | 影响 | 修复 |
|------|------|------|
| Host 头端口未归一化 | `Host: example.com:8080` 无法匹配只配 `example.com` 的站点 → **监听非 80/443 端口时所有请求 404**（多端口监听完全失效，TLS 路径必现，因为 HTTP/2 的 `:authority` 一定带端口） | 匹配前 `strip_port()`，支持 `example.com:8080` 与 `[::1]:8080`，并补单元测试 |
| `certificate::init()` 返回错误的 `JoinHandle` | `main.rs` 保存的是已结束的 `init()` 任务句柄，`abort()` 停不掉调度器 | 改为返回真正的调度器句柄 |

---

## P0（事故级）

### P0-1 访问日志"先删后写"，DB 写入失败即永久丢日志

**证据**：[gateway/src/access.rs:256-380](gateway/src/access.rs#L256-L380)

```rust
let logs = ACCESS_REQUEST_LOGS.clone();
...
ACCESS_REQUEST_LOGS.retain(|k, _| k > &current_time);   // ① 先从内存删除
get_database().insert_batch_access_requests(logs).await?; // ② 后写库
```

6 处刷盘函数全部是这个顺序：`sync_access_request_logs`、`sync_access_response_logs`、`sync_request_size_logs`、`sync_response_size_logs`、`sync_access_response_increase_size_logs`、`sync_access_request_increase_size_logs`。

**为什么是事故**：① 与 ② 之间没有任何保护。写库失败（连接池 30s 超时、PG 重启、锁等待、参数超限）时数据已从内存消失；上层 `sync()`（[:170-244](gateway/src/access.rs#L170-L244)）只打一条 ERROR 日志就继续，**不重试、不回滚**。下一轮刷盘已无数据。

**后果**：访问日志与流量统计静默丢失且无告警；面板指标长期偏低，安全审计数据缺口。

**修复方向**：写库成功后再删内存；失败批次回滚到缓冲区并退避重试；对连续失败告警。

---

### P0-2 批量 INSERT 未分块，撞 PostgreSQL 65535 参数上限

**证据**：[crates/shared/src/database/access.rs:209-365](crates/shared/src/database/access.rs#L209-L365)

```rust
builder.push_values(requests.iter(), |mut b, req| {
    b.push_bind(req.id).push_bind(&req.host) /* ... 共 10 个绑定 */;
});
builder.build().execute(&self.pool).await?;   // 单条 INSERT，无分块
```

全文**没有任何** `chunks` / `chunk_by` / 分批逻辑（已 grep 确认）。

PostgreSQL 扩展查询协议的 Bind 消息用 `Int16` 表示参数个数，**上限 65535**。据此换算：

| 函数 | 每行绑定数 | 单批上限行数 | 触发条件 |
|------|-----------|-------------|---------|
| `insert_batch_access_requests` | 10 | 6,553 | **>6553 请求/秒** |
| `insert_batch_access_request_increase_size_logs` | 4 | 16,383 | 约 100 个并发大文件下载（16KB/帧） |
| `update_batch_access_request_size_logs` | 3 | 21,845 | 同上量级 |

**为什么是事故**：超限时整条语句报错，配合 **P0-1** → 该秒全部日志永久丢失。触发点恰好是流量高峰/被攻击时，也就是最需要日志的时刻。

**修复方向**：按 500～1000 行分块提交（并保持语句总数可控）。

---

### P0-3 QPS 视图强制全表扫描 + 热点表无清理策略（"数据库优化问题"根因）

**证据**：

- 视图定义 [assets/sqls/access_init.sql:54-70](assets/sqls/access_init.sql#L54-L70)
- 查询实现 [crates/shared/src/database/access.rs:46-72](crates/shared/src/database/access.rs#L46-L72)
- 前端轮询 [dashboard/frontend/src/pages/Dashboard/statistics/QPS.vue:169](dashboard/frontend/src/pages/Dashboard/statistics/QPS.vue#L169)（`INTERVAL_MS = 5000`）

```sql
CREATE OR REPLACE VIEW qps_per_second AS
    SELECT date_trunc('second', requested_at) AS time,
           COUNT(req.id) AS total_requests, COUNT(req.id) AS qps
    FROM access_request_logs req
    GROUP BY time          -- 全表聚合
    ORDER BY time DESC;    -- 冗余排序
```

```rust
"SELECT time, total_requests, qps FROM qps_per_second
 WHERE time >= NOW() - INTERVAL '1 second' * $1 ORDER BY time DESC LIMIT $1"
```

三个叠加问题：

1. **谓词非 sargable**：`time` 是 `date_trunc('second', requested_at)` 的别名，`WHERE time >= ...` 无法使用 `idx_requested_at` 索引 → **每次都是全表扫描 + 聚合 + 排序**。
2. **表无限增长**：`access_request_logs` / `access_response_logs` 及两张 size 表**没有分区、没有 TTL、没有任何清理任务**（已 grep 确认无 `DELETE FROM access*` / `PARTITION BY` / `pg_cron`）。
3. **轮询频率固定 5s**：面板每 5 秒扫一次全表，且 `qps_per_5s` / `get_access_info`（`metrics.vue` 每 60s）也都是对同一批表的重量级聚合。

**后果**：数据量增长 → 单次查询从毫秒变秒级 → 连接池占满 → 面板全站超时 → PG CPU 饱和。这是你感受到的"数据库被抢垮"的直接原因。

**实测负载量级**（每个打开的统计页面）：

| 定时器 | 周期 | 端点 | 单页负载 |
|--------|------|------|---------|
| [QPS.vue:169](dashboard/frontend/src/pages/Dashboard/statistics/QPS.vue#L169) | 5s | `/access/qps` → `qps_per_5s` | **720 次全表聚合/小时** |
| [metrics.vue:152](dashboard/frontend/src/pages/Dashboard/statistics/metrics.vue#L152) | 60s | `/access/info` → 1/7/30 天聚合 | 60 次/小时 |

单页合计约 **0.22 req/s**，其中 12/13 是全表聚合。**25 个挂机大屏 ≈ 5 次全表聚合/秒**——足以打满一台 PostgreSQL 的 CPU。叠加 P0-11 的重试放大后可再 ×3。

**修复方向**：

- 查询改为直接过滤原始列：`WHERE requested_at >= NOW() - INTERVAL '1 second' * $1`，绕开 `date_trunc` 包裹；
- 去掉视图内多余的 `ORDER BY`；
- 用**增量计数表**（每秒 INSERT 一行）替代实时聚合，QPS 查询变成点查；
- 对日志表按天分区 + 定期 `DROP PARTITION` 保留 N 天。

---

### P0-4 刷盘循环在 DB 变慢时自激（正反馈）

**证据**：[gateway/src/access.rs:149-167](gateway/src/access.rs#L149-L167)

```rust
let next_time = (TimeDelta::seconds(1) - (updated_time - *last_time))
    .max(TimeDelta::microseconds(100))     // 下限 100µs
    .to_std()?;
if next_time.is_zero() { ...; continue; }   // 死代码：max(100µs) 保证非零
```

当 `sync()` 耗时超过 1 秒（P0-3 导致），`1s - 耗时` 为负，被 `.max(100µs)` 钳到 **100 微秒**。循环几乎不睡眠地继续发起下一轮 6 次数据库操作 → DB 更慢 → 下一轮更慢。

**后果**：与 P0-3 形成正反馈，是"数据库突然被拖垮"而非"缓慢劣化"的机制。原本用于保护的下限值反而成了加速器。

**修复方向**：设**最小间隔下限**（如 1s）而非上限式钳制；`sync()` 耗时超阈值时跳过错过的整秒并记录告警；限制单轮批量大小。

---

### P0-5 上游连接池归还未读完响应的连接 → 响应串包 / 跨站点数据泄露

**证据**：

- [gateway/src/upstream/connection.rs:284-293](gateway/src/upstream/connection.rs#L284-L293)（`Drop` 无条件归还）
- [gateway/src/upstream/connection.rs:253-260](gateway/src/upstream/connection.rs#L253-L260)（归还前只调 `is_healthy`）
- [gateway/src/upstream/connection.rs:70-76](gateway/src/upstream/connection.rs#L70-L76)

```rust
pub async fn is_healthy(&mut self) -> bool {
    matches!(timeout(Duration::from_millis(100), self.inner.write(&[])).await, Ok(Ok(_)))
}
```

`write(&[])` 对 TCP 在 tokio 中**不产生任何系统调用**，直接返回 `Ok(0)`，因此 `is_healthy()` **恒为 `true`**。健康检查形同虚设。

```rust
impl Drop for PooledUpstreamConnection {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            tokio::spawn(async move { pool.return_connection(conn).await; });  // 无条件归还
        }
    }
}
```

归还路径**不检查响应体是否已读完、不 drain**。

**攻击/故障场景**：客户端在响应传输中途断开（或超时）→ `CResponse::Incoming` 被 drop → 上游 socket 中仍残留未读的响应字节 → 该 socket 仍被放回 `idle` 队列 → 下一个请求复用它 → **读到上一个请求的响应**。

**后果**：请求 A 的响应被返回给请求 B。若 A、B 属于不同站点/用户，即为**跨租户数据泄露**；也是典型的 HTTP 响应走私面。这属于事故级安全问题。

**修复方向**：只有在响应体被完整消费（hyper 的 `Connection` future 正常结束）后才允许归还；否则直接 `close()`。移除无意义的 `write(&[])` 健康检查，改用可探测 EOF 的手段（`try_read` / `poll_read`）。

---

### P0-6 连接池无上限，且连接实际要等 120s 才归还

**证据**：

- [gateway/src/state.rs:44-47](gateway/src/state.rs#L44-L47)：`UpstreamConnectionPoolConfig::new_from_targets(addrs).url(url)` —— **从未调用 `.max_connections(..)`**，`max_connections` 保持 `0`
- [gateway/src/upstream/connection.rs:184-188](gateway/src/upstream/connection.rs#L184-L188)：`max_connections == 0` → `Semaphore::MAX_PERMITS`（≈ `usize::MAX >> 3`），等于**无上限**
- [gateway/src/upstream.rs:346-353](gateway/src/upstream.rs#L346-L353)：`connection.with_upgrades()` 在独立 task 中存活，超时 **120s** 才结束

连接只有在 `Connection` future 结束后才经 `Drop` 归还。keep-alive 场景下 `Connection` 会一直存活到 120s 超时。因此稳态下**每个并发请求各占一条上游连接**，池中几乎无连接可复用。

**后果**：fd 与上游连接数随并发线性增长，上游服务连接耗尽；`max_connections` 配置项形同虚设。

**修复方向**：显式设置合理的 `max_connections`；响应体读完后立即主动归还（`return_to_pool`），而不是依赖 `Drop`。

---

### P0-7 两个进程并发执行 DDL → 启动竞态 + 触发器真空期丢配置

**证据**：

- 两个进程都跑完整初始化：[gateway/src/main.rs:20](gateway/src/main.rs#L20)、[dashboard/backend/src/main.rs:25](dashboard/backend/src/main.rs#L25) → [crates/shared/src/database.rs:265-273](crates/shared/src/database.rs#L265-L273)
- DDL 清单：[crates/shared/src/database.rs:45-82](crates/shared/src/database.rs#L45-L82)、[websites.rs:18-37](crates/shared/src/database/websites.rs#L18-L37)、[certificate.rs:21-45](crates/shared/src/database/certificate.rs#L21-L45)、[access_init.sql](assets/sqls/access_init.sql)
- 触发器重建：[crates/shared/src/database.rs:152-175](crates/shared/src/database.rs#L152-L175)
- 兜底轮询被注释：[gateway/src/sync.rs:34-38](gateway/src/sync.rs#L34-L38)

**问题 A：并发 DDL 竞态（"多个程序抢一个表"）**
`gateway` 与 `dashboard` 启动时都会执行 `CREATE TABLE IF NOT EXISTS` / `CREATE INDEX IF NOT EXISTS` / `CREATE OR REPLACE VIEW`。PostgreSQL 的 `IF NOT EXISTS` **不做并发保护**：两个会话同时通过存在性检查时，其中一个会抛
`duplicate key value violates unique constraint "pg_type_typname_nsp_index"` 或 `tuple concurrently updated` → **启动失败**。两者同时重启（如 docker compose 整体拉起）时命中概率最高。

**问题 B：触发器真空期**
`create_trigger_notify` 是 4 条**独立语句、无事务**：

```rust
format!("DROP TRIGGER IF EXISTS {table_name}_notify ON {table_name};"),
format!("CREATE TRIGGER {table_name}_notify AFTER INSERT OR UPDATE ..."),
```

`DROP` 与 `CREATE` 之间存在**触发器真空期**。此窗口内对 `websites` / `certificates` / `dns_providers` 的写入**不会发 `pg_notify`**。

而网关唯一的兜底机制（周期性全量同步）恰好被注释掉了（`sync.rs:34-38`）。因此：

> **在真空期修改站点配置 → 网关永久不同步，直到重启。**

由于 `dashboard` 每次启动都会重放同样的 DDL，**每次重启面板都会制造这个窗口**。

**修复方向**：DDL 只由一个进程（或独立的 migrate 步骤）执行，并用 `pg_advisory_lock` 串行化；触发器用**单条事务**重建（`CREATE OR REPLACE TRIGGER` 或 `BEGIN; DROP; CREATE; COMMIT;`）；恢复周期性兜底同步作为最终一致性保障。

---

### P0-8 站点/证书删除后不从内存移除 → 已下线配置继续生效

**证据**：

- 触发器仅覆盖 INSERT/UPDATE：[crates/shared/src/database.rs:157-160](crates/shared/src/database.rs#L157-L160)（**无 DELETE 触发器**）
- 站点同步只有 `insert`：[gateway/src/sync/websites.rs:45](gateway/src/sync/websites.rs#L45)、[:60](gateway/src/sync/websites.rs#L60)、[:79](gateway/src/sync/websites.rs#L79)
- 证书同步只有 `insert`：[gateway/src/sync/cert.rs:71-77](gateway/src/sync/cert.rs#L71-L77)
- 监听器永不清理：[gateway/src/upstream.rs:57](gateway/src/upstream.rs#L57)、[:111-127](gateway/src/upstream.rs#L111-L127)
- 作者已知残留：[gateway/src/sync.rs:96](gateway/src/sync.rs#L96) `// maybe need clean LINKED_WEBSITES`

| 操作 | 后果 |
|------|------|
| 删除站点 | `WEBSITES`/`FULL_WEBSITES`/`LAZY_WEBSITES` 无删除路径 → **网关继续代理已删除的域名** |
| 修改站点 hosts | 旧域名条目残留（只 insert 不 delete）→ **旧域名仍可访问** |
| 删除/吊销证书 | `FULL_CERTIFICATES`/`LAZY_CERTIFICATES` 只增不减 → **已吊销证书继续被使用** |
| 站点移除端口 | `LISTENERS` 只增不减 → **端口持续监听** |

**后果**：下线、吊销、迁移域名等操作不生效，属配置正确性与安全问题（已删除站点可能继续对外提供本应下线的服务）。

**修复方向**：改为**对账（reconcile）式同步**——每轮同步后移除本地多余的 key；端口监听器按当前端口集合做 diff 后关闭多余监听；补 DELETE 触发器或改为全量对账。

---

### P0-9 触发器用 `NOW()`（事务开始时间）作水位 → 更新永久丢失

**证据**：

- 触发器函数 [crates/shared/src/database.rs:70-77](crates/shared/src/database.rs#L70-L77)：`NEW.updated_at = NOW();`
- 水位查询 [websites.rs:61-71](crates/shared/src/database/websites.rs#L61-L71)：`WHERE updated_at > $1`
- 水位推进 [gateway/src/sync/websites.rs:38-40](gateway/src/sync/websites.rs#L38-L40)、[:83-87](gateway/src/sync/websites.rs#L83-L87)；[cert.rs:64-66](gateway/src/sync/cert.rs#L64-L66)、[:80-84](gateway/src/sync/cert.rs#L80-L84)

`NOW()` 返回的是**事务开始时间**，不是提交时间。失效序列：

| 时刻 | 事件 |
|------|------|
| T=100 | 事务 A **开始** |
| T=100 | 网关完成一轮同步，`LAST_SYNC` 推进到 100 |
| T=105 | 事务 A 提交，更新站点 Y，但 `updated_at = NOW() = 100` |
| T=106 | 提交触发 `pg_notify` → 网关同步 → `WHERE updated_at > 100` → **匹配不到 Y** |

**后果**：站点/证书更新随机丢失，且**无法自愈**（通知只触发同一水位查询），必须重启网关。

**修复方向**：改用 `clock_timestamp()`；或采用单调递增序列 / `pg_current_xact_id()` 作为水位；或每次通知直接做全量对账（表规模小，代价可接受）。

---

### P0-10 SNI 预读无上限 → 单连接可申请 16MB，慢速攻击耗尽内存

**证据**：

- [crates/protocols/src/tls.rs:52-57](crates/protocols/src/tls.rs#L52-L57)：`handshake_len` 由 3 字节大端构成，最大 `0xFFFFFF` ≈ **16MB**，且被直接作为 `WantMoreData(n)` 返回
- [gateway/src/upstream/protocols.rs:36-58](gateway/src/upstream/protocols.rs#L36-L58)：`data.extend(stream.pre_read_buf(n).await?)`
- [crates/shared/src/streams.rs:135-149](crates/shared/src/streams.rs#L135-L149)：`pre_read_buf` 每次 `vec![0u8; size]` 重新分配

攻击者只需发送一个声明 `handshake_len = 16MB` 的 ClientHello 头，网关就会为其分配并等待读取 16MB。该预读路径**没有任何超时**（`header_read_timeout(30s)` 只作用于 hyper 层，预读发生在其之前）。并发发起即可耗尽内存与 fd。

注：解析函数本身的越界检查是完善的（逐段 `data.len() < pos + n` 校验），问题只在于**长度上限缺失**。

**修复方向**：对 `handshake_len` 设硬上限（正常 ClientHello 远小于 16KB）；对预读阶段加超时；限制单连接预读总字节数。

---

### P0-11 前端 ky 默认重试在数据库吃紧时把请求放大 3 倍（放大器）

**证据**：[dashboard/frontend/src/constant.ts:11-45](dashboard/frontend/src/constant.ts#L11-L45) 创建 `got` / `gotWithAuth` 时**未覆盖 `retry` 与 `timeout`**；[QPS.vue:152-170](dashboard/frontend/src/pages/Dashboard/statistics/QPS.vue#L152-L170) 吞掉异常后**无条件重排下一次轮询**

ky 的默认策略是 `limit: 2`（共 3 次尝试），对 `408/413/429/500/502/503/504` 重试，退避 `0.3 * 2^(n-1)` 秒。

```ts
} catch (error) {
    // console.error('获取 QPS 数据失败', error);
    // 失败时不更新 data，保留旧数据
}
clearTimeout(task.value);
...
task.value = setTimeout(refreshQPS, delay);   // 失败也会继续排下一轮
```

**失效链路**：数据库吃紧 → `/access/qps` 返回 500/502/503 → ky 自动重试 2 次 → **一次轮询变成 3 次全表聚合** → 数据库更吃紧 → 下一轮轮询失败 → 无限循环。这与 P0-3、P0-4 是同一个正反馈回路的前端一侧。

**修复方向**：统计类 GET 显式设置 `retry: 0`（或 `limit: 1` 且仅对幂等安全状态码重试）；失败时指数退避而非固定 5s 重排；连续失败达阈值后暂停轮询并提示用户。

---

### P0-12 前端定时器无 `visibilitychange` 门控，隐藏标签页持续打库

**证据**：全量 grep `src/` 中 `visibilitychange` / `document.hidden` / `pagehide` → **0 匹配**。受影响定时器：[QPS.vue:169](dashboard/frontend/src/pages/Dashboard/statistics/QPS.vue#L169)（5s）、[metrics.vue:152](dashboard/frontend/src/pages/Dashboard/statistics/metrics.vue#L152)（60s）、[bindTotp.vue:80](dashboard/frontend/src/components/console/bindTotp.vue#L80)（300s）

**后果**：被切到后台、最小化或长期无人看的统计页面仍在持续发起全表聚合。挂机的大屏/多标签页会线性叠加负载。仅依赖浏览器的后台定时器节流（Chrome 约 5 分钟后才降到 ~1 次/分钟，且开着 DevTools 时完全不节流）不可靠。

**修复方向**：统一封装一个 `useVisiblePolling`，用 `document.visibilityState` 在隐藏时暂停、恢复时立即刷新一次。

---

### P0-13 证书续签判定方向写反 → 证书到期后才开始续签，HTTPS 会中断

**证据**：[crates/shared/src/database/certificate.rs:94-99](crates/shared/src/database/certificate.rs#L94-L99)

```sql
SELECT id, name, hostnames, dns_provider_id FROM certificates
WHERE (expires_at IS NULL OR expires_at < (NOW() - '7 days'::INTERVAL))
  AND dns_provider_id IS NOT NULL AND email IS NOT NULL
```

`NOW() - '7 days'::INTERVAL` 是 **7 天前**。因此条件是"**证书已经过期超过 7 天**"，而不是"证书将在 7 天内到期"。

| 证书状态 | 是否触发续签 | 期望 |
|---------|------------|------|
| 3 天后到期 | ❌ 否 | ✅ 应续签 |
| 已过期 1 天 | ❌ 否 | ✅ 应续签 |
| 已过期 8 天 | ✅ 是 | ✅ 应续签 |

**后果**：所有自动签发的证书会在**到期后**才被尝试续签，且要再等 7 天。对于 `expires_at < NOW()` 的证书，这期间站点 HTTPS **完全中断**（网关只能回落到默认自签证书，浏览器证书告警）。这是可预期的线上事故。

**修复方向**：改为 `expires_at < NOW() + '7 days'::INTERVAL`（到期前 7 天续签）。同时为 `expires_at` 建索引——该查询每 30 秒执行一次（[certificate.rs:24](dashboard/backend/src/certificate.rs#L24)）且当前是全表扫描。

---

### P0-14 证书签发在"已过期 7 天"与"从未签发"之间无幂等保护，多实例下重复消耗 ACME 配额

**证据**：[dashboard/backend/src/certificate.rs:20-57](dashboard/backend/src/certificate.rs#L20-L57)、[:94-104](dashboard/backend/src/certificate.rs#L94-L104)

`PENDINGS` 是**进程内的** `HashMap`（[:20-21](dashboard/backend/src/certificate.rs#L20-L21)）。去重只在单进程内生效：

```rust
PENDINGS.write().await.insert(id, handle);   // 进程内
```

**问题 A：多副本/滚动重启期间重复签发**
若 `dashboard` 以多副本部署，或滚动更新时新旧实例并存，两个实例都会判定同一张证书需要签发，**同时向 ZeroSSL 发起 ACME 签发**。ZeroSSL 对证书数量有严格配额，重复申请会直接耗尽配额，导致后续所有证书都无法签发。同时二者会互相覆盖 `fullchain` / `private_key`（`update_certificate` 是无条件 UPDATE）。

**问题 B：`PENDINGS` 条目泄漏 → 该证书永不再续签**
清理动作在 `sign()` 末尾：

```rust
pub async fn sign(cert: NeedSignCertificate) {
    if let Err(e) = inner_sign(cert).await { ... }   // 错误被捕获，OK
    let mut pendings = PENDINGS.write().await;
    pendings.remove(&id);                            // 仅在正常路径执行
}
```

若签发任务**panic**（acmex 内部 `unwrap`、`.expect`）或 `PENDINGS.write().await` 本身失败，则 `remove` 不执行，且该任务已从 `get_expired` 的过滤中被永久排除（[:36-41](dashboard/backend/src/certificate.rs#L36-L41)）→ **该证书再也不会被重试**，静默到期。

**问题 C**：`certificate::init()` 内部用 `tokio::spawn` 起了 `tokio_schedule` 任务后立即返回（[:23-30](dashboard/backend/src/certificate.rs#L23-L30)），而 `main.rs` 保存的 `auto_cert` JoinHandle 指向的是**已经结束的** `init()` 任务 → 关闭时的 `auto_cert.abort()` 无法停止调度器（[main.rs:58-70](dashboard/backend/src/main.rs#L58-L70)）。

**修复方向**：用数据库侧状态（如 `signing_started_at` 字段 + `SELECT ... FOR UPDATE SKIP LOCKED`）做跨进程互斥；`PENDINGS.remove` 放进 `Drop` 守卫或 `finally` 语义块，保证 panic 路径也清理，并对失败设置重试上限与退避。

---

## P1（严重）

### P1-1 生产构建下访问地图恒为空

**证据**：[dashboard/backend/src/router/access.rs:34-38](dashboard/backend/src/router/access.rs#L34-L38)

```rust
#[cfg(not(debug_assertions))]
{
    return APIResponse::ok(HashMap::new());
}
```

`cargo build --release` 会关闭 `debug_assertions`，因此**生产环境 `/access/access_map` 永远返回空对象**，前端地图永远无数据。疑似临时熔断或未完成特性，需确认。（另：`get_requests_of_ips` 对 `remote_addr` 做 `GROUP BY`，但该列**没有索引**——`access_init.sql` 只建了 `requested_at`/`website_id`/`status` 索引，debug 下即全表扫描。）

### P1-2 上游 TLS 从未启用，HTTP/2 上游不支持

**证据**：

- [gateway/src/state.rs:44-47](gateway/src/state.rs#L44-L47)：构造池时**未调用 `.tls(..)`**，`config.tls` 保持 `false`
- [gateway/src/upstream/connection.rs:244-251](gateway/src/upstream/connection.rs#L244-L251)：`if self.config.tls { .. } else { UpstreamConnection::new_tcp(addr) }`
- [gateway/src/upstream.rs:341](gateway/src/upstream.rs#L341)：硬编码 `client::conn::http1::Builder`

后果：配置成 `https://` 的后端**不会建立 TLS**，一律走明文 TCP；上游 HTTP/2 不受支持。同时 TLS 相关配置字段（`tls`、`tls_config`、`hostname`）成为死代码。

### P1-3 连接级 300s 硬超时截断 WebSocket / 长请求

**证据**：[gateway/src/upstream.rs:205](gateway/src/upstream.rs#L205) `timeout(Duration::from_secs(300), conn)`；请求级 60s 超时 [upstream.rs:302](gateway/src/upstream.rs#L302)

`timeout(300s)` 包住了**整条连接**，包含升级后的 WebSocket 长连接。WebSocket 会在 5 分钟后被强制断开；任何超过 60s 的请求（大文件、SSE、长轮询）直接返回 504。

### P1-4 JWT `exp` 与 `exp_at` 单位不一致（秒 vs 分钟）

**证据**：[dashboard/backend/src/auth/jwt.rs:21](dashboard/backend/src/auth/jwt.rs#L21) 与 [:30](dashboard/backend/src/auth/jwt.rs#L30)

```rust
exp:    now + *EXPIRES,                              // 秒
exp_at: now + chrono::Duration::minutes(*EXPIRES),   // 分钟
```

`EXPIRES` 默认 `60*60*24*7 = 604800`（秒）。于是令牌实际 **7 天**有效，却告诉客户端 **420 天**。客户端会长期持有已失效令牌，造成 401 重试与刷新逻辑混乱。

### P1-5 上游路径拼接可 panic，且 base path 被丢弃

**证据**：[gateway/src/upstream.rs:365-378](gateway/src/upstream.rs#L365-L378)、[:450-458](gateway/src/upstream.rs#L450-L458)

```rust
pool.get_path().map_or_else(
    || origin_req.uri().path().to_string(),
    |v| { let a = v.join(&origin_req.uri().path()[1..]).unwrap(); ... },
)
```

1. **可被远端触发的 panic**：`get_path()` 在 `state.rs` 中必然返回 `Some`（`.url(url)` 总是被调用），因此 `[1..]` 分支**总会执行**。对 authority-form 请求（如 `CONNECT host:port`）`uri.path()` 为空串，`&""[1..]` **越界 panic**。
2. **base path 语义错误**：`Url::join` 遵循 RFC 3986，对无尾斜杠的 base 会替换最后一段。后端配置为 `http://h/api` 时，`join("foo/bar")` 得到 `http://h/foo/bar`——**`/api` 前缀被静默丢弃**。
3. `.unwrap()` 在 join 失败时 panic。

### P1-6 证书通配符匹配在握手热路径上编译正则

**证据**：[gateway/src/sync/cert.rs:125-129](gateway/src/sync/cert.rs#L125-L129)

```rust
fn regex_match(host: &str, pattern: &str) -> bool {
    let pattern = pattern.replace('.', "\\.").replace('*', r"[-\w]+");
    let re = Regex::new(&format!("^{pattern}$")).unwrap();   // 每次调用都编译
    re.is_match(host)
}
```

`lookup_certificate` 对每个候选模式调用它。每次未命中缓存的新 SNI 握手都要编译 N 个正则。对比 [websites.rs:49-52](gateway/src/sync/websites.rs#L49-L52) 已改为预编译并存入 `LAZY_WEBSITES`，`cert.rs` **未同步该优化**，且 `LAZY_CERTIFICATES` 存的是原始字符串而非 `Regex`。

### P1-7 `is_healthy()` 在持锁期间 await → 连接池级串行化

**证据**：[gateway/src/upstream/connection.rs:199-214](gateway/src/upstream/connection.rs#L199-L214)

```rust
let mut idle = self.idle.lock().await;          // 持有互斥锁
if let Some(mut conn) = idle.pop_front() {
    if conn.is_healthy().await {                 // 锁内 await，最长 100ms
```

所有并发取连接请求在同一把 `tokio::sync::Mutex` 上排队。叠加 P0-6（永不归还），该锁成为全站吞吐瓶颈。

### P1-8 监听任务 panic 被静默吞掉 + 重复 bind

**证据**：[gateway/src/upstream.rs:111-127](gateway/src/upstream.rs#L111-L127)、[:66-109](gateway/src/upstream.rs#L66-L109)

```rust
let thread = tokio::spawn(async move {
    let listener = CustomDualStackTcpListener::new_by_port(port).await.unwrap();  // panic 点
    ...
});
LISTENERS.insert(port, thread);   // JoinHandle 存入后从不检查
```

- 端口 bind 失败时任务 panic，但 `LISTENERS` 仍标记该端口"已监听" → 该站点**静默不服务**，日志中无有效线索。
- `contains_key` → `spawn` → `insert` 不是原子操作；并发调用（启动同步与 NOTIFY 处理同时触发）可重复 bind。因 `listener.rs` 开启了 `SO_REUSEPORT`，**两次 bind 都会成功**，连接被内核分摊到两个 accept 循环。

### P1-9 QPS 查询参数无上限校验

**证据**：[crates/shared/src/models/access.rs:234-238](crates/shared/src/models/access.rs#L234-L238)（`count: usize` 无校验）、[dashboard/backend/src/router/access.rs:17](dashboard/backend/src/router/access.rs#L17)（直接透传）

`inline?count=100000000` 会先做全表聚合再 `LIMIT 100000000`，单请求即可放大数据库负载。对比 [router/log.rs:26](dashboard/backend/src/router/log.rs#L26) 有 `min(limit, 100)` 保护，`access` 路由完全没有。

### P1-10 初始化 DDL 每次启动重复执行且持有重量级锁

**证据**：[crates/shared/src/database/access.rs:26](crates/shared/src/database/access.rs#L26) `sqlx::raw_sql(INIT_SQL)`；[access_init.sql](assets/sqls/access_init.sql) 含 11 条 `CREATE INDEX IF NOT EXISTS`、3 条 `CREATE OR REPLACE VIEW`、4 条 `CREATE TABLE IF NOT EXISTS`

两个进程每次启动都会在**热表**上重放整段 DDL。`CREATE OR REPLACE VIEW` 需要 ACCESS EXCLUSIVE 锁，与面板自身的 QPS 查询互斥；`CREATE INDEX`（即使是 `IF NOT EXISTS`）也会短暂申请表锁，与网关每秒的批量 INSERT 竞争。

### P1-11 统计页日期切换无防抖、无取消，响应可乱序覆盖

**证据**：[Statistics.vue:32-36](dashboard/frontend/src/pages/Dashboard/Statistics.vue#L32-L36) → [metrics.vue:154-159](dashboard/frontend/src/pages/Dashboard/statistics/metrics.vue#L154-L159)

```ts
watch(() => props.in_days, () => { refreshInfo(); });   // 立即请求，无防抖、无取消
```

全量 grep 确认代码中**没有任何 `AbortController` / `signal`**。快速切换 1天/7天/30天会叠加多个未取消的 30 天全量聚合；慢响应可能**乱序返回并覆盖** `data.value`，界面显示与所选区间不一致。切换区间也不会影响 QPS 轮询。

### P1-12 日志分页产生 N+1 请求风暴

**证据**：[Log.vue:41-49](dashboard/frontend/src/pages/Dashboard/settings/Log.vue#L41-L49)（`watch(currentPage)` → `refresh()`）、[:50-66](dashboard/frontend/src/pages/Dashboard/settings/Log.vue#L50-L66)（`Promise.all` 中逐条 `getUserInfo(user_id)`）

每次翻页 = 1 次 `/logs/total` + 1 次 `/logs/page` + **最多 10 次 `/auth/info`** ≈ 12 个请求，且无防抖、无取消。连点翻页或长按方向键会按点击次数成倍叠加（并叠加 P0-11 的 ky 重试）。用户信息有进程内缓存与在途去重（[apis/user.ts:4-39](dashboard/frontend/src/apis/user.ts#L4-L39)），可缓解重复请求但不解决首屏突发。

### P1-13 访问地图：数据被丢弃、watcher 是死代码、`in_days` 未透传

**证据**：[AccessMap.vue:58-65](dashboard/frontend/src/pages/Dashboard/statistics/AccessMap.vue#L58-L65)、[Statistics.vue:43](dashboard/frontend/src/pages/Dashboard/Statistics.vue#L43)

```ts
async function refresh() {
    const resp = await get_access_map(props.in_days, type.value);
    console.log(resp);          // 结果被丢弃，从未赋给 data
}
watch(() => type.value, debounce(refresh, 500));   // type 无任何 UI 可变更 → 永不触发
```

且 `Statistics.vue` 渲染 `<AccessMap>` 时**未传 `:in_days`** → 恒为 1 天。配合 P1-1，生产构建下该接口直接短路返回空，debug 下则执行 `GROUP BY remote_addr` 全表扫描 + 每个 IP 一次的 GeoIP 查询。属于"花了代价、结果没用上"。

### P1-14 站点列表无分页，且每次弹窗关闭都重新拉全量

**证据**：[websites.vue:88-98](dashboard/frontend/src/pages/Dashboard/websites.vue#L88-L98)、[:102-109](dashboard/frontend/src/pages/Dashboard/websites.vue#L102-L109)、[apis/websites.ts:16-21](dashboard/frontend/src/apis/websites.ts#L16-L21)

`getWebsites()` = `GET /websites`，**不带任何分页参数**，全表返回；随后又拉取全站点当日聚合指标。`onMounted` 与每次"添加站点"弹窗关闭都会重跑。非定时器驱动，但站点数量增长后每次操作都是全量聚合。

### P1-15 IDOR：任意已认证用户可读取其他用户信息

**证据**：[dashboard/backend/src/auth.rs:84-92](dashboard/backend/src/auth.rs#L84-L92)

```rust
pub async fn get_userinfo(
    AuthJWTInfoExtract(_): AuthJWTInfoExtract,   // 已认证，但身份被丢弃
    Query(info): Query<AuthQueryInfo>,
) -> APIResponse<AuthInfo> {
    let user = get_database().get_user_from_id(&info.user_id).await;   // 用请求参数里的 id
```

**问题**：中间件只校验"请求者持有合法令牌"，随后**丢弃请求者身份**，改用查询串里的 `user_id` 去查库。因此任何持有有效令牌的用户都可以传任意 `user_id` 读取他人信息，无需与自身身份匹配。

**影响面**：[models/auth.rs:127-133](dashboard/backend/src/models/auth.rs#L127-L133) 的 `AuthInfo` 仅含 `id` / `username` / `created_at` / `updated_at` / `bound_totp`，**不包含 `jwt_secret` / `totp_secret`**（`AuthInfo::from` 已正确过滤），因此泄露的是**用户 ID 与用户名（可枚举）**，而非凭据。仍属越权访问。

**附注**：前端 `Log.vue` 逐条调用 `/auth/info?user_id=X` 正是依赖了这个行为（见 P1-12）；修复时应一并改为服务端 join 或批量接口。

### P1-16 无角色/权限模型：任意已认证用户等同管理员，且可枚举全部账号

**证据**：[dashboard/backend/src/auth.rs:249-251](dashboard/backend/src/auth.rs#L249-L251)

```rust
async fn all_users(AuthJWTInfoExtract(_): AuthJWTInfoExtract) -> APIResponse<Vec<AuthInfo>> {
    APIResponse::result(get_database().get_info_of_users().await)
}
```

代码中**没有任何角色、权限或所有权校验**（已通读 `auth/`、`router/`、`models/auth.rs`）。任何一个成功登录的账号都可以：

- 通过 `/auth/users` 列出**全部账号**（含 `bound_totp` 状态，可用于筛选未绑定 TOTP 的目标）；
- 管理全部站点、证书与 DNS 服务商凭据（`/websites`、`/certificates`、`/dnsproviders` 仅要求"已认证"）。

这意味着**无法安全地开设低权限账号**——一旦把面板访问权交给第二个人，就等于交出全部基础设施控制权（含 DNS 服务商的 secret_key，可用于劫持任意域名）。

**修复方向**：引入显式的角色字段与授权中间件；`all_users` 至少限制为管理员；对站点/证书/DNS 凭据增加所有权或角色校验。

---

## P2（一般）

| 编号 | 问题 | 证据 |
|------|------|------|
| P2-1 | 视图内 `ORDER BY time DESC` 冗余（外层已排序），且使视图难以被优化 | [access_init.sql:61](assets/sqls/access_init.sql#L61)、[:70](assets/sqls/access_init.sql#L70) |
| P2-2 | `metrics.vue` 轮询接口抛异常时不再重排定时器 → 轮询**静默永久停止**，且产生未处理 rejection | [metrics.vue:148-153](dashboard/frontend/src/pages/Dashboard/statistics/metrics.vue#L148-L153) |
| P2-3 | 请求路径上的 `unwrap()`：`ResponseLog::new(...).unwrap()`、`RequestLog::new` 内 `try_into().unwrap()`、`get_database_time().unwrap()` | [upstream.rs:283-294](gateway/src/upstream.rs#L283-L294)、[access.rs:65](gateway/src/access.rs#L65)、[access.rs:110](gateway/src/access.rs#L110) |
| P2-4 | `ip2region` 分支被注释停用，仍保留文件与依赖 | [dashboard/backend/src/ip.rs:18-22](dashboard/backend/src/ip.rs#L18-L22) |
| P2-5 | `middle_refresh_token` 对每个 200 响应都重新签发令牌（含一次 `get_user` 查询），形成无绝对上限的滑动会话 | [dashboard/backend/src/auth.rs:104-123](dashboard/backend/src/auth.rs#L104-L123) |
| P2-6 | 每个认证请求两次用户表查询（校验一次 + 签发刷新一次） | [auth/jwt.rs:15-32](dashboard/backend/src/auth/jwt.rs#L15-L32)、[auth.rs:40-52](dashboard/backend/src/auth/jwt.rs#L40-L52) |
| P2-7 | `crates/acme` 硬编码绝对路径 `include_str!("/develop_workspaces/...")`、调试 `println!`、未使用字段 | [crates/acme/src/main.rs](crates/acme/src/main.rs)、[lib.rs:22-26](crates/acme/src/lib.rs#L22-L26) |
| P2-8 | CI 只构建镜像，无 `cargo fmt --check` / `clippy` / 前端 `eslint` 门禁 | [.github/workflows/build.yml](.github/workflows/build.yml) |
| P2-9 | 已停用的 `gateway/src/proxy/` 与多个空文件仍在树中 | `gateway/src/proxy/`、`gateway/src/dns.rs`、`gateway/src/foundation.rs` |
| P2-10 | `bindTotp.vue` 的 300s 轮询**在绑定成功后不清除**，停留在"已验证"界面会一直每 5 分钟申请新 TOTP 密钥 | [bindTotp.vue:80-89](dashboard/frontend/src/components/console/bindTotp.vue#L80-L89)、[:103](dashboard/frontend/src/components/console/bindTotp.vue#L103)、[:109-111](dashboard/frontend/src/components/console/bindTotp.vue#L109-L111) |
| P2-11 | QPS 请求 60 个点、后端返回 300 行（`count*5`）每 5 秒序列化一次，图形只用 ≤60 个 | [crates/shared/src/database/access.rs:60-72](crates/shared/src/database/access.rs#L60-L72)、[QPS.vue:119-149](dashboard/frontend/src/pages/Dashboard/statistics/QPS.vue#L119-L149) |
| P2-12 | 多个页面分页完全失效（无 load 影响，但功能坏）：`Certificates.vue` 未绑定分页事件；`DNSProviders.vue`、`users.vue` 绑定了事件但从不重新拉取；`Log.vue` 的 `@page-size` 只改 `perPage` 不触发刷新 | `websites/Certificates.vue:2`、`websites/DNSProviders.vue:5-6`、`console/users.vue:2-7`、`Log.vue:47-49` |

---

## 待确认（需人工判断，无法从代码定论）

1. **P0-3 的修复优先级**：`access_request_logs` 的预期数据量与保留期是多少？若需要长期保留，必须上分区；若只需 7 天，定时清理即可。
2. **P1-1**：`access_map` 在 release 下返回空是**有意的熔断**（因 IP 查询太贵）还是遗留 bug？
3. **P0-6**：`max_connections` 的合理取值？需要按上游服务容量确定。
4. **P0-7**：是否接受"只有 dashboard 执行 DDL、gateway 只读"的约定？这会改变部署顺序依赖。
5. **P0-5**：是否已在生产观察到过"返回了别的站点内容"的现象？若有，可确认该路径已被触发。
6. **P1-2**：`https://` 上游后端是否已在生产配置？若已配置，说明流量实际走了明文或一直失败。
7. **P0-2**：线上峰值 QPS 与并发下载量级？用于确认 6553/16383 行阈值是否已被突破。
8. **P0-10**：网关是否直接暴露在公网？决定该 DoS 向量的实际风险等级。

---

## 建议的修复顺序

> 状态：下表「立即（止损）」与「紧急」三行**已执行完毕**（2026-10-02 第三轮），
> 详见上方「修复状态」。剩余行为后续迭代。

| 阶段 | 内容 | 理由 | 状态 |
|------|------|------|------|
| 立即（止损） | P0-1 改"先写后删"；P0-2 加分块；P0-4 循环改最小 1s 间隔；**P0-13 证书续签方向改正** | 改动小、直接止住数据丢失、自激与 HTTPS 中断 | ✅ 已完成 |
| 紧急（安全） | P0-5 禁止不安全归还；P0-10 限制 `handshake_len`；P1-15/P1-16 修 IDOR 与补权限模型 | 数据泄露与越权，成本可控 | ✅ 已完成 |
| 紧急（前端放大器） | P0-11 统计请求 `retry: 0`；P0-12 轮询加 `visibilitychange` 门控 | 一两行改动即可显著降低库压 | ✅ 已完成 |
| 本迭代 | P0-3 查询改写为直接过滤 + 建分区/清理；P0-7 DDL 单进程 + advisory lock + 恢复兜底同步 | 消除雪崩根因 | 🟡 查询与 DDL 已修，**分区/清理未做** |
| 本迭代 | P0-8 改为对账式同步；P0-9 水位改 `clock_timestamp()`；P0-6 设连接池上限 | 配置不生效类故障 | ✅ 已完成 |
| 本迭代 | P0-14 证书签发跨进程互斥 + `PENDINGS` 清理加固 | 避免 ACME 配额耗尽 | ✅ 已完成 |
| 后续 | P1-2/P1-3/P1-12/P1-14；补 CI 门禁与回归测试 | 质量与可维护性 | ⬜ 待做 |

---

## 附：对 [CONVERSATION.md](CONVERSATION.md) 迁移方案的交叉核对

[CONVERSATION.md](CONVERSATION.md) 是另一 AI 会话产出的《访问日志存储优化 + v1→v2 迁移方案》。以下为对照本仓库**实际代码**逐条核对的结论。

### ✅ 方案已覆盖的本报告问题

| 本报告 | 方案对应措施 |
|--------|-------------|
| P0-7 两进程并发 DDL | ① 三条铁律：DDL 只在 `--migrate` 入口；② 全部 DDL 走 `pg_advisory_xact_lock`；③ `schema_version` 表 + 服务侧只 `verify` |
| P0-3 视图全表扫描 | 废弃视图改用**参数化函数**，谓词直接落在 `requested_at` 上（sargable），配合按周分区裁剪 |
| P0-3 表无清理策略 | 按周 RANGE 分区 + `ensure_upcoming_weeks` + `DETACH PARTITION CONCURRENTLY` |
| P1-1 `remote_addr` 缺索引 | 新增 `idx_req_remote_time ON (remote_addr, requested_at)` |
| P1-10 每次启动重放 DDL | 服务启动只 `verify_database_schema()`，DDL 收敛到迁移 Job |
| P0-2（部分）批量 UPDATE | `CASE WHEN` 改 `FROM (VALUES ...)`，绑定数由 3/行降到 2/行 |

### ❌ 方案未覆盖的本报告问题（需另行修复）

方案聚焦**存储层**，以下问题与分区无关，**不会因迁移而消失**：

| 本报告 | 说明 |
|--------|------|
| **P0-1** 访问日志"先删后写" | 纯 Rust 缓冲逻辑缺陷（`gateway/src/access.rs`），迁移不涉及。**这是当前唯一持续在丢数据的 bug** |
| **P0-2** 未分块 | `FROM (VALUES ...)` 只把上限从 21,845 行提到 32,767 行，**仍会撞 65535 上限**，必须加分块 |
| **P0-4** 刷盘循环自激 | `.max(100µs)` 未提及；DB 变慢时的正反馈仍在 |
| **P0-13** 证书续签方向写反 | 一行 SQL 修复，与存储无关，但会导致 HTTPS 中断 |
| **P0-11 / P0-12** 前端放大器 | 方案**完全未涉及前端**。分区让单次查询变快，但 ky 默认重试 ×3 与隐藏标签页持续轮询仍在 |
| P0-5/6/8/9/10、P1-15/16 | 连接池、内存对账、水位、SNI 预读、权限模型 —— 均在设计范围之外 |

> 结论：方案能消除**雪崩的放大器**，但**不能止血**。P0-1/P0-2/P0-4/P0-13 应在迁移前先行修复，否则迁移期间的批量写入仍会丢数据。

### ⚠️ 方案中的技术风险与不准确处

**1. 移除外键会拆掉现有的一层保护网（重要）**
现状 `access_response_logs.id` 既是主键又是外键（[access_init.sql:16](assets/sqls/access_init.sql#L16)）。移除 FK 后，若"请求行写入失败、响应行写入成功"（正是 **P0-1** 的失效形态），会产生**静默孤儿行**：`LEFT JOIN ... ON req.id = resp.id` 匹配不上，统计里无声消失。**保留 FK 时这类问题会直接报错暴露。**
→ 建议：**先修 P0-1（写入顺序 + 分块），再移除 FK**；或改为写入后跑一次孤儿行对账。

**2. `get_access_info` 的响应表无法分区裁剪**
新函数中 `LEFT JOIN access_response_logs resp ON req.id = resp.id` **只有 id 条件，没有分区键条件**，因此 `resp` 侧无法裁剪，需扫描全部保留分区（1~2 月 ≈ 8~9 个周分区）。方案声称"视图无法裁剪分区"，但对此 join 同样成立。
→ 建议：补 `AND resp.responsed_at >= NOW() - make_interval(days => p_days)`（语义安全，因 `responsed_at >= requested_at`）。

**3. 主键改为 `(id, 分区键)` 后，`id` 不再是全局唯一**
方案保留了 `idx_resp_id` 供 join 使用，但**约束层面已允许同一 `id` 出现在不同分区**。ObjectId 自带时间戳，实践中落在同一分区，但语义弱化。若 join 命中多行，`COUNT` 会重复计数。
→ 建议：明确记录该假设，或对 `id` 加全局唯一性校验任务。

**4. 回滚步骤不完整**
方案的回滚只做"DROP 新表 + RENAME 老表"，但迁移中已 `DROP` 三个 FK 且"DROP 老表普通索引（保留主键索引）"。回滚后**索引与外键不会自动恢复**，性能与完整性都回不到迁移前。
→ 建议：回滚清单里补 `CREATE INDEX`（原 11 条）与三个 `ADD CONSTRAINT`。

**5. 字段对齐描述与实际代码不符（会误导实现者）**
方案称"Rust `DatabaseQPS.qps`"，但实际 [models/access.rs:173-185](crates/shared/src/models/access.rs#L173-L185) 中 `DatabaseQPS` **只有 `count` 与 `time` 两个字段**，`count` 由 `total_requests` 映射，**根本不存在 `qps` 字段**。
→ 按现状，SQL 只需 SELECT 出 `total_requests` 即可，Rust 侧无需改动；`AS qps` 是多余的。此外 `get_access_info` 新函数把 size 列类型从 `uint8` 改为 `numeric`，而 Rust 侧 `query_as::<_, (.., USize, USize)>` 依赖 `uint8`，**这里需要同步改类型**，方案未提及。

**6. 待确认：PostgreSQL 版本无法从仓库验证**
方案以 "PostgreSQL 18" 为前提（`DETACH PARTITION CONCURRENTLY` 需 PG 14+）。但 [docker-compose.yml:17](docker-compose.yml#L17) 用的是 `${POSTGRES_IMAGE_PREFIX}/postgres:${POSTGRES_TAG:-latest}`，[.env.default](.env.default) 中 `POSTGRES_TAG=latest` 且指向自建镜像源 —— **实际版本无法从仓库判定**。若镜像低于 PG 14，方案第 6.2 节与 DETACH 策略全部失效。**待确认。**

**7. 其他需要注意的点**
- **时区一致性成为硬要求**：分区边界由 `date_trunc('week', NOW())::date` 在会话时区下计算（部署设 `TZ=Asia/Shanghai`）。迁移 Job 与服务的时区必须一致，否则可能触发 `no partition of relation found for row`。
- **`get_requests_of_ips` 代价与用途不匹配**：即使加了 `(remote_addr, requested_at)` 索引，N 天窗口内按 IP 全量 GROUP BY 依旧昂贵；而该接口在生产（release）下被 [router/access.rs:34-38](dashboard/backend/src/router/access.rs#L34-L38) 短路为空、前端拿到结果也只 `console.log` 丢弃（见 P1-13）。**先确认这个功能是否还要**，再决定是否为其优化。
- **停机窗口 5~15 分钟偏乐观**：2kw 行 × 4 张表的逐周回填 + 回填后建索引，取决于磁盘与并发度；方案未给出"停写"的具体操作方式（停 gateway？drain？）。建议演练一次并备好回滚脚本。

### 建议的执行顺序（与方案 P0~P3 的差异）

| 顺序 | 内容 | 理由 |
|------|------|------|
| ① 先做（与迁移无关） | P0-1 写入顺序、P0-2 分块、P0-4 循环下限、P0-13 证书方向、P0-11/P0-12 前端 | **止血**，改动小，且必须在移除 FK 之前完成 |
| ② 再做 | 方案 P0：`locks.rs` + `with_ddl_lock` + `schema_version` + 启动模式拆分 | 消除 DDL 竞态（对应本报告 P0-7） |
| ③ 然后 | 方案 P1：v2 DDL 落地 + 仓储层改函数 + 触发器白名单 | 消除 P0-3 全表扫描 |
| ④ 最后 | 方案 P2/P3：冷迁移脚本、分区 worker、预聚合表 | 数据量迁移与长期维护 |

---

## 相关文档

- 项目总览：[agent.md](agent.md)
- 待办清单：[TODO.md](TODO.md)
- 变更日志：[CHANGELOG.md](CHANGELOG.md)
- 存储优化 / v1→v2 迁移方案（外部 AI 产出，待评审）：[CONVERSATION.md](CONVERSATION.md)
