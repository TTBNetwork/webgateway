# WebGateway v2 数据库设计（主控已实施）

> 本文是数据库设计与实施记录。主控（dashboard）、迁移与 shared 的就绪等待已按此实现；
> 网关二进制（含上线/下线标记）在下一步接入。
>
> 目标库是测试库 `webgateway_beta_v2`；连接串不写入仓库，只通过 `DATABASE_URL` 或本地 `config.toml` 注入。
> 撰写时实测环境：PostgreSQL 18.2；库为空（仅 `plpgsql` 扩展）；角色 `webgateway` 是库 owner + superuser；
> `public` schema 可建对象；UTF8 + C collation；`TimeZone` 已统一为 `UTC`（数据库级默认，面板按用户时区展示）。

## 1. 目标与约束

1. **只有主控初始化**：`dashboard-backend`（主控）负责连接、执行迁移、写就绪标记；
   `gateway`（网关）**永不执行任何 DDL**，启动时只读探测，初始化完成前不进入服务。
2. **网关等待主控**：主控还没初始化、或正在迁移时，网关必须等待；等待有默认超时和明确日志，
   超时失败时给出可操作报错，而不是静默挂死。
3. **表规范**：每张表都有 `created_at`；可更新的表有 `updated_at`（由触发器自动维护）；
   配置类表通过 `LISTEN/NOTIFY` 发出变更通知，供网关热同步。
4. **v2 是全新库，不迁移 v1 数据**（已确认）：不继承 v1「访问日志 schema 版本门控 + DROP 历史数据」的逻辑；
   迁移只做向前建表/建索引/建函数，不搬历史数据。
5. **先做主控**（已确认）：本步交付 workspace、shared 数据库层、迁移与 dashboard 主控；
   网关二进制及其上线/下线标记在下一步接入。CRUD/仓储层、访问日志写入与汇总调度、认证、ACME、
   前端对接留给后续任务。
6. **使用内部 PG 镜像自带的扩展**：`uint128`（无符号整数类型族）与 `btree_gin`，由主控在迁移里创建；
   镜像定义见仓库根目录 `postgresql-dockerfile`。
7. **网关在线状态**（优先级中等）：网关启动时在 `gateways` 表标记 online 并保持心跳，优雅退出时标记 offline；
   `gateways_online` 视图以心跳新鲜度判定在线，避免崩溃后状态残留。
8. **统计粒度**（已确认）：最近 1 小时保留秒级聚合，超过 1 小时归并为分钟级；日级由分钟级汇总。
9. **时区**（已确认）：默认 `UTC+0`，数据库/服务端存储与统计统一 UTC；面板按用户时区展示。

## 2. 环境与连接

| 项 | 值 |
|---|---|
| 数据库 | PostgreSQL 18.2 |
| 库 | `webgateway_beta_v2` |
| 角色 | `webgateway`（库 owner、superuser） |
| 编码 / 排序 | UTF8 / C |
| 时区 | `UTC`（数据库/会话默认；面板按用户时区展示） |
| 当前对象 | 无表、无视图、仅 `plpgsql` |
| 可用扩展 | `uint128 1.1.1`、`btree_gin 1.3`、`pg_cron 1.6`、`vchord 0.0.0`（当前均未安装） |

- 内部镜像：`postgresql-dockerfile`（`postgres:18-bookworm` + Pigsty `postgresql-18-pg-uint128` 等）。
  本步只用其中两个扩展：`uint128`（无符号整数类型族）与 `btree_gin`，由主控在迁移里 `CREATE EXTENSION IF NOT EXISTS`。
- 连接串只通过环境变量 `DATABASE_URL` 或本地 `config.toml` 提供；`.gitignore` 忽略 `config.toml`，
  仓库里只提交 `config.example.toml`。
- 驱动：`sqlx 0.8`（`postgres` + `runtime-tokio` + `chrono` + `migrate`），不启用 TLS feature（与 v1 一致）。
  sqlx 会把连接会话的 `TimeZone` 固定为 `UTC`；数据库级默认也设为 `UTC`（镜像 `CMD` 与 `ALTER DATABASE`）。
  如果以后要求 `sslmode=require`，需要额外启用 sqlx 的 rustls feature。
- 连接池：`max_connections = 10`（可配）、`acquire_timeout = 5s`、单次连接超时 `connect_timeout = 10s`。

## 3. 启动时序（核心需求）

### 3.1 就绪标记表

```sql
CREATE TABLE schema_state (
    id         SMALLINT PRIMARY KEY DEFAULT 1 CHECK (id = 1),  -- 单行表
    version    INTEGER NOT NULL,                               -- 已应用的 schema 版本
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

约定：

| 网关探测结果 | 含义 | 行为 |
|---|---|---|
| 表不存在（SQLSTATE `42P01`） | 主控从未初始化 | 等待 |
| 表存在但行不存在 | 迁移未完成 | 等待 |
| `version < 期望版本` | 主控正在迁移到新版本 | 等待 |
| `version >= 期望版本` | 初始化完成 | 就绪 |
| 连接失败 / 认证失败 / 超时 | 数据库不可用 | 等待（计入总超时） |
| 其它 SQL 错误（权限、语法等） | 非预期状态 | 立即失败，不吞错误 |

「期望版本」= 当前二进制内嵌迁移目录中的最大版本号（编译期嵌入，见 3.2）。

### 3.2 主控初始化流程

```text
连接池 -> SET LOCAL lock_timeout -> pg_advisory_xact_lock(SCHEMA_INIT)
      -> sqlx Migrator 执行未应用的迁移（每个迁移文件一个事务）
      -> 迁移文件末尾调用 wg_mark_schema_version(N) 更新 schema_state
      -> 提交（释放 advisory lock）
      -> 只读校验 schema（标记版本 + 必需表/列）
      -> 进入服务
```

关键点：

- **迁移文件自带就绪标记**：每个迁移 SQL 的最后一条是 `SELECT wg_mark_schema_version(N);`，
  与建表在**同一个事务**里提交。因此网关一旦看到版本 N，表结构必定已经完整提交，
  不存在“表建好了但标记还没写”的中间态。
- **并发安全**：主控在事务级 advisory lock 下执行迁移，多实例启动时串行；
  `lock_timeout = 60s`（可配），拿不到锁时快速失败并给出可操作报错，避免无限挂起（v1 踩过这个坑）。
  sqlx 迁移器自身的锁在此时不会竞争。
- **锁 ID**：`SCHEMA_INIT = 0x5747_5632_0000_0001`（ASCII `WGV2` + `0001`），避免与其它应用冲突。
- **不提供独立 `--migrate` 模式**：主控启动即迁移；如需独立迁移 Job，后续再加参数。
- **主控连接失败即退出**：不在主控里做无限重试，启动顺序交给编排层的健康检查/重启策略。
  （如需“等 PG 起来”，后续可加 `connect_retry` 配置。）

### 3.3 网关等待流程

```text
构建 lazy 连接池（数据库没起来也不 panic）
loop:
    SELECT version FROM schema_state WHERE id = 1
    - Ok(version >= 期望)     -> 就绪，返回连接池
    - Ok(version <  期望)     -> 继续等（主控在迁移）
    - Err(42P01 表不存在)     -> 继续等（主控未初始化）
    - Err(连接/认证/超时)     -> 继续等
    - Err(其它)               -> 立即失败
    退避：1s -> 2s -> 3s -> 4s -> 5s 封顶；日志限频（状态变化或每 10s 打一条）
```

- 默认总超时 **300s**；`DB_WAIT_TIMEOUT=0` 表示无限等待。
- 超时错误包含：已等待时长、最后观测状态（连接失败/表不存在/版本号）、期望版本、排查建议
  （主控是否启动、`DATABASE_URL` 是否正确、主控日志）。
- 版本**高于**期望（网关二进制比主控旧）：打 WARN 后继续，避免滚动升级时卡死。
- 就绪后网关再做一次**只读**校验（必需表存在），然后进入服务；全程不执行 DDL。
- 就绪后网关不再轮询该表；后续配置同步走配置表的 `*_updater` 通知（见第 5 节）。

### 3.4 为什么不用 advisory lock / NOTIFY 作为就绪信号

- 锁只用于让**主控之间**串行迁移；网关如果先启动先拿到锁，会误以为“初始化完成”。
- NOTIFY 是瞬时事件，网关恰好在主控发通知之后启动就会漏掉，仍需要持久标记兜底。
- 因此就绪判断以 `schema_state` 持久标记为准；轮询成本可忽略（启动期一次性）。

## 4. 表结构

### 4.1 公共约定

- 所有对象位于 `public`；表名、列名全小写 snake_case。
- 主键 `id` 为 `TEXT`，存 24 位十六进制 ObjectId（由后续 `crates/simple_shared` 生成）；
  `configurations` 的主键是业务 key（`TEXT`）。
- 时间列一律 `TIMESTAMPTZ`，默认 `now()`；`updated_at` 由 `wg_update_updated_at()` 触发器维护
  （用 `clock_timestamp()` 而不是 `now()`，避免长事务提交后更新时间早于同步水位）。
- 每张表都有 `created_at`；只插入不更新的表（`users_client_secrets`、`web_log`、四张访问日志表）
  不设 `updated_at`。
- **使用内部镜像自带的扩展**（`postgresql-dockerfile`）：
  - `uint128` 扩展提供无符号整数族 `uint1/uint2/uint4/uint8/uint16`（按**字节数**命名，`uint16` 才是 128 位）。
    `websites.ports` 用 `uint2[]`、`body_length` 用 `uint8`、响应状态用 `uint2`，与 v1 语义一致。
  - `btree_gin` 提供 TEXT 的 GIN opclass：`websites.name` 的 `GIN (name)` 依赖它；`hosts TEXT[]` 的 GIN 用内置数组 opclass。
  - 两个扩展由主控在迁移开头创建（实测 PostgreSQL 18.2 支持事务内 `CREATE EXTENSION`，随迁移一起回滚/提交），幂等。
  - 前提：使用内部镜像；若主控角色不是 superuser/库 owner，需由 DBA 预先安装这两个扩展
    （`CREATE EXTENSION IF NOT EXISTS` 随后变成空操作）。
- 外键只保留真正需要的：`certificates.dns_provider_id`、`users_client_secrets.user_id`、`web_log.user_id`。
- 权限分层（后续可落地）：主控使用可执行 DDL 的角色；网关使用**没有 DDL 权限**的角色（读写由 GRANT 限定），
  从权限层再次保证「只有主控能初始化」。本步先用同一个角色跑通流程。

### 4.2 表清单

| 表 | 用途 | 更新? | NOTIFY |
|---|---|---|---|
| `schema_state` | 迁移版本 / 就绪标记（单行） | 是 | - |
| `stats_state` | 统计聚合水位（单行） | 是 | - |
| `configurations` | 控制面配置（JSONB key/value） | 是 | `configurations_updater` |
| `users` | 控制面用户（TOTP/JWT/角色） | 是 | - |
| `users_client_secrets` | 用户额外绑定的 TOTP secret | 否 | - |
| `users_info`（视图） | 用户信息 + 绑定统计 | - | - |
| `web_log` | 系统/认证操作日志 | 否 | - |
| `dns_providers` | DNS 服务商凭据与域名 | 是 | `dns_providers_updater` |
| `certificates` | 证书（手动上传 / ACME 申请） | 是 | `certificates_updater` |
| `websites` | 站点路由与后端配置 | 是 | `websites_updater` |
| `gateways` | 网关节点注册与在线/下线标记 | 是 | - |
| `access_request_logs` | 请求主日志（周分区） | 否 | - |
| `access_response_logs` | 响应主日志（周分区） | 否 | - |
| `access_request_size_logs` | 请求体按秒累加（周分区） | 否 | - |
| `access_response_size_logs` | 响应体按秒累加（周分区） | 否 | - |
| `access_stats_second` | 最近 1 小时的秒级聚合 | 是 | - |
| `access_stats_minute` | 超过 1 小时的分钟级聚合 | 是 | - |
| `access_stats_daily` | 按天按站点预聚合 | 是 | - |

### 4.3 表定义

#### schema_state

| 列 | 类型 | 约束/默认 | 说明 |
|---|---|---|---|
| `id` | `SMALLINT` | PK, `DEFAULT 1`, `CHECK (id = 1)` | 单行约束 |
| `version` | `INTEGER` | NOT NULL | 已应用迁移的最大版本号 |
| `created_at` | `TIMESTAMPTZ` | NOT NULL `DEFAULT now()` | |
| `updated_at` | `TIMESTAMPTZ` | NOT NULL `DEFAULT now()` | 由 `wg_mark_schema_version()` 更新 |

只有 `wg_mark_schema_version(int)` 会写这张表。

#### configurations

| 列 | 类型 | 约束/默认 | 说明 |
|---|---|---|---|
| `key` | `TEXT` | PK | 配置键（统一小写，应用层保证） |
| `value` | `JSONB` | NOT NULL `DEFAULT '{}'` | 配置内容 |
| `created_at` / `updated_at` | `TIMESTAMPTZ` | NOT NULL `DEFAULT now()` | |

- 触发器：`configurations_updated_at`、`configurations_notify`。
- 用途示例：`access_log_retention`、`access_logs_schema_version` 等（后续任务写入）。

#### users

| 列 | 类型 | 约束/默认 | 说明 |
|---|---|---|---|
| `id` | `TEXT` | PK | ObjectId |
| `username` | `TEXT` | NOT NULL | 用户名 |
| `totp_secret` | `TEXT` | NOT NULL | 主 TOTP secret（v1 明文存储，v2 暂沿用，后续可加密） |
| `jwt_secret` | `TEXT` | NOT NULL | 每用户独立 JWT secret |
| `role` | `TEXT` | NOT NULL `DEFAULT 'view'`, `CHECK (role IN ('admin','user','view'))` | 权限角色 |
| `token_version` | `INTEGER` | NOT NULL `DEFAULT 0` | JWT 撤销计数（迁移 0005） |
| `last_login` | `TIMESTAMPTZ` | | |
| `last_ip` | `TEXT` | | |
| `addresses` | `TEXT[]` | NOT NULL `DEFAULT '{}'` | |
| `created_at` / `updated_at` | `TIMESTAMPTZ` | NOT NULL `DEFAULT now()` | |

- 唯一索引：`users_username_lower_idx ON users (LOWER(username))`（大小写不敏感唯一）。
- 触发器：`users_updated_at`。

#### users_client_secrets

| 列 | 类型 | 约束/默认 | 说明 |
|---|---|---|---|
| `user_id` | `TEXT` | NOT NULL，`REFERENCES users(id) ON DELETE CASCADE` | |
| `secret` | `TEXT` | NOT NULL | 额外 TOTP secret |
| `created_at` | `TIMESTAMPTZ` | NOT NULL `DEFAULT now()` | |

- 唯一约束 `(user_id, secret)`；索引 `(user_id)`；索引 `(secret)`（登录时反查）。

#### users_info（视图）

```sql
CREATE VIEW users_info AS
SELECT id, username, totp_secret, jwt_secret, role,
       created_at, updated_at, last_login, last_ip, addresses,
       (SELECT COUNT(*) FROM users_client_secrets WHERE user_id = users.id) AS client_secrets_count,
       EXISTS (SELECT 1 FROM users_client_secrets cs WHERE cs.user_id = users.id) AS bound_totp
FROM users;
```

#### web_log

| 列 | 类型 | 约束/默认 | 说明 |
|---|---|---|---|
| `id` | `TEXT` | PK | ObjectId |
| `user_id` | `TEXT` | NOT NULL，`REFERENCES users(id)` | 操作者 |
| `content` | `JSONB` | NOT NULL `DEFAULT '{}'` | |
| `address` | `TEXT` | NOT NULL | 来源地址 |
| `created_at` | `TIMESTAMPTZ` | NOT NULL `DEFAULT now()` | |

- 索引：`(user_id, created_at DESC)`、`(created_at DESC)`（面板分页）。

#### dns_providers

| 列 | 类型 | 约束/默认 | 说明 |
|---|---|---|---|
| `id` | `TEXT` | PK | ObjectId |
| `name` | `TEXT` | | 显示名 |
| `provider` | `JSONB` | NOT NULL | 服务商类型与凭据 |
| `domains` | `TEXT[]` | NOT NULL `DEFAULT '{}'` | 可用域名 |
| `created_at` / `updated_at` | `TIMESTAMPTZ` | NOT NULL `DEFAULT now()` | |

- 索引：`(created_at DESC)`（分页）。
- 触发器：`dns_providers_updated_at`、`dns_providers_notify`。

#### certificates

| 列 | 类型 | 约束/默认 | 说明 |
|---|---|---|---|
| `id` | `TEXT` | PK | ObjectId |
| `name` | `TEXT` | | 显示名 |
| `hostnames` | `TEXT[]` | NOT NULL `DEFAULT '{}'` | 证书 SAN |
| `fullchain` | `TEXT` | | 证书链 PEM |
| `private_key` | `TEXT` | | 私钥 PEM（列表接口需脱敏） |
| `dns_provider_id` | `TEXT` | `REFERENCES dns_providers(id) ON DELETE SET NULL` | 自动申请使用 |
| `email` | `TEXT` | | ACME 邮箱 |
| `expires_at` | `TIMESTAMPTZ` | | 供续签扫描 |
| `signing_started_at` | `TIMESTAMPTZ` | | 跨进程签发认领标记（10 分钟租约） |
| `auto_renew` | `BOOLEAN` | NOT NULL `DEFAULT FALSE` | 自动签发/续期（迁移 0004） |
| `dns_zone` | `TEXT` | | DNS-01 父区域（迁移 0004） |
| `last_error` | `TEXT` | | 最近一次签发失败原因（迁移 0004） |
| `created_at` / `updated_at` | `TIMESTAMPTZ` | NOT NULL `DEFAULT now()` | |

- 索引：`(expires_at)`（续签扫描）、`(dns_provider_id)`、`(created_at DESC)`、
  partial `(expires_at NULLS FIRST) WHERE auto_renew`（迁移 0004）。
- 触发器：`certificates_updated_at`、`certificates_notify`。

#### websites

| 列 | 类型 | 约束/默认 | 说明 |
|---|---|---|---|
| `id` | `TEXT` | PK | ObjectId |
| `name` | `TEXT` | | 显示名 |
| `hosts` | `TEXT[]` | NOT NULL `DEFAULT '{}'` | 域名匹配表（支持通配符） |
| `ports` | `uint2[]` | NOT NULL `DEFAULT '{}'` | 监听端口（`uint2` 天然限制 0..65535，应用层再校验 1..65535） |
| `certificates` | `TEXT[]` | NOT NULL `DEFAULT '{}'` | 关联证书 id（元数据） |
| `backends` | `JSONB` | NOT NULL `DEFAULT '[]'` | 上游列表 |
| `config` | `JSONB` | NOT NULL `DEFAULT '{}'` | 站点行为配置（后续含网关缓存策略、IP/UA 拦截规则等） |
| `created_at` / `updated_at` | `TIMESTAMPTZ` | NOT NULL `DEFAULT now()` | |

- 索引：`GIN (hosts)`、`GIN (name)`、`(created_at DESC)`。
- 触发器：`websites_updated_at`、`websites_notify`。

#### gateways

| 列 | 类型 | 约束/默认 | 说明 |
|---|---|---|---|
| `id` | `TEXT` | PK | ObjectId |
| `machine_id` | `TEXT` | NOT NULL, UNIQUE | `machine_id` crate 生成的节点唯一 id |
| `name` | `TEXT` | | 显示名 |
| `country` / `city` | `TEXT` | | 节点归属（用于响应头展示节点） |
| `address` | `TEXT` | | 节点地址（IP/host） |
| `version` | `TEXT` | | 网关二进制版本 |
| `status` | `TEXT` | NOT NULL `DEFAULT 'offline'`, `CHECK (status IN ('online','offline'))` | 上线/下线标记 |
| `last_seen_at` | `TIMESTAMPTZ` | | 心跳时间 |
| `started_at` | `TIMESTAMPTZ` | | 最近一次上线时间 |
| `stopped_at` | `TIMESTAMPTZ` | | 最近一次优雅下线时间 |
| `created_at` / `updated_at` | `TIMESTAMPTZ` | NOT NULL `DEFAULT now()` | |

- 索引：`(status, last_seen_at DESC)`。
- 触发器：`gateways_updated_at`。
- 视图 `gateways_online`：`status = 'online' AND last_seen_at >= now() - INTERVAL '30 seconds'`。
  网关崩溃时不会执行下线标记，因此在线判断必须叠加心跳新鲜度（心跳间隔计划 10s 左右）。
  网关已实现：启动注册 online、每 10s 心跳更新 `last_seen_at`、SIGTERM/Ctrl-C 优雅退出标记 offline。

#### access_request_logs（按周分区）

`PARTITION BY RANGE (requested_at)`，分区名 `<表名>_<IYYY>_w<IW>`。

| 列 | 类型 | 约束/默认 | 说明 |
|---|---|---|---|
| `id` | `TEXT` | NOT NULL | ObjectId（请求 id） |
| `host` | `TEXT` | NOT NULL | 请求 Host |
| `method` | `TEXT` | NOT NULL | |
| `path` | `TEXT` | NOT NULL | |
| `headers` | `JSONB` | NOT NULL `DEFAULT '[]'` | |
| `http_version` | `TEXT` | NOT NULL | |
| `remote_addr` | `TEXT` | NOT NULL | 客户端 IP |
| `body_length` | `uint8` | NOT NULL `DEFAULT 0` | |
| `website_id` | `TEXT` | | 命中站点，可为空 |
| `created_at` | `TIMESTAMPTZ` | NOT NULL `DEFAULT now()` | 写入时间 |
| `requested_at` | `TIMESTAMPTZ` | NOT NULL `DEFAULT now()` | 分区键 |

- PK `(id, requested_at)`（分区表主键必须包含分区键）。
- 索引：`(requested_at)`、`(website_id, requested_at)`、`(remote_addr, requested_at)`、`(id)`。

#### access_response_logs（按周分区）

`PARTITION BY RANGE (responsed_at)`。

| 列 | 类型 | 约束/默认 | 说明 |
|---|---|---|---|
| `id` | `TEXT` | NOT NULL | 请求 id |
| `status` | `uint2` | NOT NULL | 不加 RFC 范围 CHECK：上游可能返回非标准状态码，仍要如实记录 |
| `headers` | `JSONB` | NOT NULL `DEFAULT '[]'` | |
| `body_length` | `uint8` | | 响应体长度，未知时为空 |
| `http_version` | `TEXT` | NOT NULL | |
| `website_id` | `TEXT` | | |
| `created_at` | `TIMESTAMPTZ` | NOT NULL `DEFAULT now()` | |
| `responsed_at` | `TIMESTAMPTZ` | NOT NULL `DEFAULT now()` | 分区键 |
| `backend_responsed_at` | `TIMESTAMPTZ` | | 上游响应时间；失败请求为空（v1 默认 `NOW()`，语义不对，v2 改为可空） |

- PK `(id, responsed_at)`。
- 索引：`(responsed_at)`、`(status, responsed_at)`、`(website_id, responsed_at)`、`(id)`。

#### access_request_size_logs / access_response_size_logs（按周分区）

`PARTITION BY RANGE (at_second)`；按「请求 id + 秒」累加，保证唯一。

| 列 | 类型 | 约束/默认 | 说明 |
|---|---|---|---|
| `id` | `TEXT` | NOT NULL | ObjectId |
| `request_id` / `response_id` | `TEXT` | NOT NULL | 父日志 id |
| `body_length` | `uint8` | NOT NULL `DEFAULT 0` | 该秒累计字节 |
| `at_second` | `TIMESTAMPTZ` | NOT NULL `DEFAULT now()` | 分区键（整秒） |
| `created_at` | `TIMESTAMPTZ` | NOT NULL `DEFAULT now()` | |

- PK `(id, at_second)`；唯一索引 `(request_id, at_second)` / `(response_id, at_second)`；
  索引 `(at_second)`。

#### access_stats_second（最近 1 小时）

| 列 | 类型 | 约束/默认 | 说明 |
|---|---|---|---|
| `second_at` | `TIMESTAMPTZ` | NOT NULL | 整秒 |
| `website_id` | `TEXT` | NOT NULL `DEFAULT '__unknown__'` | 无站点归属的哨兵值 |
| `total_requests` / `total_responses` | `BIGINT` | NOT NULL `DEFAULT 0` | |
| `total_ips` | `BIGINT` | NOT NULL `DEFAULT 0` | 该秒独立 IP |
| `e4xx_requests` / `e5xx_requests` | `BIGINT` | NOT NULL `DEFAULT 0` | |
| `backend_error_requests` | `BIGINT` | NOT NULL `DEFAULT 0` | |
| `total_requests_size` / `total_response_size` | `uint8` | NOT NULL `DEFAULT 0` | |
| `created_at` / `updated_at` | `TIMESTAMPTZ` | NOT NULL `DEFAULT now()` | |

- PK `(second_at, website_id)`；索引 `(website_id, second_at DESC)`；触发器 `access_stats_second_updated_at`。
- 只保留最近 1 小时；更早的数据由主控 stats 任务归并进 `access_stats_minute` 后删除。

#### access_stats_minute（超过 1 小时）

| 列 | 类型 | 约束/默认 | 说明 |
|---|---|---|---|
| `minute_at` | `TIMESTAMPTZ` | NOT NULL | 整分钟 |
| `website_id` | `TEXT` | NOT NULL `DEFAULT '__unknown__'` | |
| `total_requests` / `total_responses` | `BIGINT` | NOT NULL `DEFAULT 0` | |
| `total_ips` | `BIGINT` | NOT NULL `DEFAULT 0` | 该分钟独立 IP |
| `e4xx_requests` / `e5xx_requests` | `BIGINT` | NOT NULL `DEFAULT 0` | |
| `backend_error_requests` | `BIGINT` | NOT NULL `DEFAULT 0` | |
| `total_requests_size` / `total_response_size` | `uint8` | NOT NULL `DEFAULT 0` | |
| `created_at` / `updated_at` | `TIMESTAMPTZ` | NOT NULL `DEFAULT now()` | |

- PK `(minute_at, website_id)`；索引 `(website_id, minute_at DESC)`；触发器 `access_stats_minute_updated_at`。

#### access_stats_daily

| 列 | 类型 | 约束/默认 | 说明 |
|---|---|---|---|
| `day` | `DATE` | NOT NULL | 数据库本地日期 |
| `website_id` | `TEXT` | NOT NULL | 无站点归属用 `__unknown__` |
| `total_requests` | `BIGINT` | NOT NULL `DEFAULT 0` | |
| `total_responses` | `BIGINT` | NOT NULL `DEFAULT 0` | |
| `total_ips` | `BIGINT` | NOT NULL `DEFAULT 0` | 每日独立 IP |
| `e4xx_requests` / `e5xx_requests` | `BIGINT` | NOT NULL `DEFAULT 0` | |
| `backend_error_requests` | `BIGINT` | NOT NULL `DEFAULT 0` | 无响应日志的请求 |
| `total_requests_size` / `total_response_size` | `uint8` | NOT NULL `DEFAULT 0` | |
| `created_at` / `updated_at` | `TIMESTAMPTZ` | NOT NULL `DEFAULT now()` | |

- PK `(day, website_id)`；索引 `(website_id, day DESC)`；触发器 `access_stats_daily_updated_at`。

## 5. 触发器与数据库函数

### 5.1 通用触发器函数

```sql
CREATE OR REPLACE FUNCTION wg_update_updated_at() RETURNS trigger AS $fn$
BEGIN
    NEW.updated_at := clock_timestamp();
    RETURN NEW;
END;
$fn$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION wg_notify_change() RETURNS trigger AS $fn$
DECLARE
    payload jsonb;
BEGIN
    payload := jsonb_build_object(
        'table', TG_TABLE_NAME,
        'id',    to_jsonb(NEW)->>'id',
        'key',   COALESCE(to_jsonb(NEW)->>'id', to_jsonb(NEW)->>'key'),
        'updated_at', to_jsonb(NEW)->>'updated_at'
    );
    PERFORM pg_notify(TG_TABLE_NAME || '_updater', payload::text);
    RETURN NEW;
END;
$fn$ LANGUAGE plpgsql;
```

- 每个可更新表的 `updated_at` 触发器 `BEFORE UPDATE` 调用 `wg_update_updated_at()`。
- 配置类表额外挂 `AFTER INSERT OR UPDATE` 的 `*_notify` 触发器，频道为 `<表名>_updater`：
  `configurations_updater`、`websites_updater`、`certificates_updater`、`dns_providers_updater`。
- 迁移 0002 增加 `AFTER DELETE` 的 `*_notify_delete` 触发器（负载带 `operation: delete`），
  删除配置时网关也能收到通知，不需要轮询。
- 通知负载示例：`{"table":"websites","id":"...","key":"...","updated_at":"2026-11-18T..."}`。
  网关收到通知后触发一次全量同步；LISTEN 连接（重）连时先比较 `(COUNT(*), MAX(updated_at))` 水位，
  相同则跳过全量同步。
- 触发器在迁移事务里 `DROP TRIGGER IF EXISTS` + `CREATE TRIGGER`，对其它会话原子可见。

### 5.2 访问日志分区函数

| 函数 | 作用 |
|---|---|
| `wg_access_partition_parents() RETURNS text[]` | 返回四张已存在的分区父表名 |
| `wg_ensure_weekly_partition(parent text, week_start date)` | 建单周分区（`<parent>_<IYYY>_w<IW>`），内部 advisory xact lock 防并发重复建 |
| `wg_ensure_upcoming_weeks(ahead int DEFAULT 2)` | 为四张父表预建「上一周 ~ 未来 N 周」分区 |
| `wg_drop_partitions_older_than(days int) RETURNS int` | 整周回收：只 DROP 整周都早于截止日的分区，`days < 90` 直接 RAISE |
| `wg_drop_expired_weekly_partitions(days int DEFAULT 7) RETURNS int` | 按访问日志保留期回收：只 DROP 整周都早于截止日的分区，`days < 7` 直接 RAISE |

- 迁移 0001 末尾调用 `wg_ensure_upcoming_weeks(2)`，迁移 0003 调用 `wg_ensure_upcoming_weeks(3)`。
- 主控 stats 任务在启动后的第一个 tick（以及随后每小时）调用 `wg_ensure_upcoming_weeks(3)` 与
  `wg_drop_expired_weekly_partitions(7)`：前者保证当前周 + 未来 3 周始终存在，后者整周 DROP
  已完全过期的分区，避免逐行 DELETE 扫描死分区。`wg_drop_partitions_older_than` 保留（>= 90 天
  安全阈值），当前代码不使用。
- 时区统一 `UTC+0`：sqlx 连接与会话都是 UTC，数据库级默认也设为 UTC；周分区按 **UTC 周**（周一起）对齐，
  `access_stats_*` 的日期也按 UTC。实测分区边界例如 `2026-11-16 08:00+08`（= UTC 周一 00:00）。
  面板根据用户时区展示（前端换算），服务端不做本地时区换算。

### 5.3 就绪标记函数

```sql
CREATE OR REPLACE FUNCTION wg_mark_schema_version(p_version INTEGER)
RETURNS void LANGUAGE plpgsql AS $fn$
BEGIN
    INSERT INTO schema_state (id, version, created_at, updated_at)
    VALUES (1, p_version, clock_timestamp(), clock_timestamp())
    ON CONFLICT (id) DO UPDATE
       SET version = EXCLUDED.version,
           updated_at = clock_timestamp();
END;
$fn$;
```

每个迁移文件的最后一条语句：`SELECT wg_mark_schema_version(<该迁移的版本号>);`。

### 5.4 统计粒度与汇总

| 表 | 粒度 | 保留 | 写入 |
|---|---|---|---|
| `access_stats_second` | 秒 | 最近 1 小时 | 每 10s 重算 `max(水位, now-90s)` ~ `now-2s` |
| `access_stats_minute` | 分钟 | 最近 1 天 | 超过 1 小时的秒行原子归并；水位缺口里超过 1 小时的部分直接从原始日志回填 |
| `access_stats_daily` | 天 | 长期 | 跨天后从分钟级行原子归并并删除分钟行 |

- 归并任务实现在 `crates/shared/src/stats.rs`（主控启动）：每 10s 一个事务，
  `pg_try_advisory_xact_lock` 保证多主控只有一个执行。
- **水位（`stats_state.last_aggregated_at`）**：每轮读取上次水位，窗口从
  `LEAST(水位, now-90s)` 开始，但总是至少重算最近 90s（延迟日志批次仍可重放，UPSERT 覆盖）。
  水位缺口超过 1 小时的部分直接按分钟从原始日志回填，最近 1 小时写入秒表，随后正常做
  秒→分→日归并；所有语句与水位推进在**同一个事务**内提交，失败不会丢水位。
  主控停机多久都能在启动后的第一轮补齐（上限为原始日志保留期 7 天）。
- 原始访问日志保留 **7 天**；每小时维护会整周 DROP 完全过期的分区，并对仍跨越窗口边界的
  分区做逐行 DELETE；聚合表只保留聚合值。
- 实测（beta 库 2026-11-19）：把水位回拨 3 小时后，2 小时前的原始日志被回填为
  `access_stats_minute` 的 1 请求 / 1 响应 / 11、22 字节；重启主控后 8 周前的旧分区被整周 DROP，
  当前周 + 未来 3 周分区存在。集成测试 `crates/shared/tests/stats_rollup.rs` 覆盖同样场景。
- `total_ips` 为每站点每秒 `COUNT(DISTINCT remote_addr)` 后再求和（跨秒会重复计数）；
  `backend_error_requests` 以 502/504 计。
- 面板查询：最近 1 小时读秒级表，1 小时内读分钟级表，超过 1 天读日级表。

## 6. 迁移方案

- 使用 **sqlx 迁移文件**（`crates/shared/migrations/0001_initial_schema.sql`、`0002_notify_deletes.sql`、
  `0003_stats_maintenance.sql`），
  编译期通过 `sqlx::migrate!("./migrations")` 嵌入二进制；部署只需可执行文件，不依赖运行目录下的 SQL 文件。
- 选择理由：
  1. 迁移有版本记录（`_sqlx_migrations`）与 checksum，历史清晰、不会重复执行；
  2. 每个迁移文件在独立事务中执行，PostgreSQL 的 DDL 事务性保证失败整体回滚；
  3. 主控独占迁移入口（只有 dashboard 二进制调用 `Migrator::run`），网关没有迁移代码路径。
- 迁移文件一经部署**不再修改**（checksum 校验会失败，fail-closed）；schema 变更新增 `0003_*.sql`，
  在文件末尾 `SELECT wg_mark_schema_version(3);`，代码中的「期望版本」取自内嵌迁移的最大版本号，自动跟上。
- 滚动升级注意：迁移应保持向后兼容（旧网关照常可读写），破坏性变更（删列/改类型）必须拆成多步迁移；
  建议先升级并等待主控迁移完成，再升级网关。
- 扩展创建放在迁移事务内（实测 PostgreSQL 18.2 允许），失败随迁移整体回滚；扩展是**数据库级**对象，
  `CREATE EXTENSION IF NOT EXISTS` 幂等。
- 0001 迁移内容顺序：
  1. `CREATE EXTENSION IF NOT EXISTS uint128;` + `CREATE EXTENSION IF NOT EXISTS btree_gin;`
  2. `schema_state` + 触发器函数 + `wg_mark_schema_version()`
  3. `configurations` + 触发器
  4. `users` / `users_client_secrets` / `users_info` / `web_log`
  5. `dns_providers` + 触发器
  6. `certificates` + 触发器
  7. `websites` + 触发器
  8. 四张访问日志分区表 + 分区函数 + `wg_ensure_upcoming_weeks(2)`
  9. `access_stats_daily` + 触发器
  10. `SELECT wg_mark_schema_version(1);`
- 0002 迁移：`wg_notify_delete()` + 四张配置表的 `AFTER DELETE` 触发器 + `SELECT wg_mark_schema_version(2);`。
- 0003 迁移：`stats_state` 单行水位表 + `updated_at` 触发器、`wg_drop_expired_weekly_partitions()`、
  `wg_ensure_upcoming_weeks(3)` + `SELECT wg_mark_schema_version(3);`。
- 0004 迁移：`certificates.auto_renew` / `dns_zone` / `last_error` + 续期 partial index
  + `SELECT wg_mark_schema_version(4);`（证书签发/上传见 `certificates.md`）。
- 0005 迁移：`users.token_version`（JWT 撤销计数）+ `SELECT wg_mark_schema_version(5);`。
- `verify_schema()`（主控迁移后、网关就绪后各跑一次，纯只读）：
  - `schema_state.version >= 期望版本`；
  - 必需表存在：`schema_state`、`configurations`、`users`、`users_client_secrets`、`web_log`、
    `dns_providers`、`certificates`、`websites`、四张访问日志表、`access_stats_daily`、`stats_state`；
  - 必需扩展已安装：`SELECT extname FROM pg_extension WHERE extname IN ('uint128','btree_gin')` 返回 2 行；
  - 关键列存在：`users.role`、`users.token_version`、`certificates.signing_started_at`、`certificates.auto_renew`、
    `certificates.dns_zone`、`certificates.last_error`、`access_request_logs.remote_addr`。

## 7. 配置项

```toml
# config.example.toml（真实 config.toml 被 .gitignore 忽略）
[database]
url = "postgres://user:password@host:5432/webgateway_beta_v2"  # 可用 DATABASE_URL 覆盖
max_connections = 10          # 连接池上限
connect_timeout_secs = 10     # 单次建立连接超时
init_lock_timeout_secs = 60   # 仅主控：等 advisory lock 的上限
wait_timeout_secs = 300       # 仅网关：等待主控初始化的总超时，0 = 无限
[server]
port = 3000                   # 仅主控 HTTP 端口
```

| 环境变量 | 覆盖项 |
|---|---|
| `DATABASE_URL` | `database.url` |
| `DATABASE_MAX_CONNECTIONS` | `database.max_connections` |
| `DATABASE_CONNECT_TIMEOUT` | `database.connect_timeout_secs` |
| `DATABASE_INIT_LOCK_TIMEOUT` | `database.init_lock_timeout_secs` |
| `DATABASE_WAIT_TIMEOUT` | `database.wait_timeout_secs` |
| `DASHBOARD_API_PORT` | `server.port` |

配置优先级：环境变量 > 当前目录 `config.toml` > 默认值；`url` 缺失时报错退出并提示设置 `DATABASE_URL`。

## 8. 代码结构（确认后实施）

```text
Cargo.toml                         # workspace: crates/shared, dashboard/backend, gateway
crates/shared/
  Cargo.toml
  migrations/0001_initial_schema.sql
  src/lib.rs
  src/logger.rs                    # tracing 初始化（英文日志）
  src/database.rs                  # DatabaseConfig / Database / 迁移 / 等待 / 校验
  tests/database_init.rs           # 需要 DATABASE_URL，否则自动跳过
dashboard/backend/
  Cargo.toml
  src/main.rs                      # 主控：连接 -> 迁移 -> 校验 -> axum /health
  src/config.rs
  src/router.rs                    # 目前只有 GET /health
  config.example.toml
gateway/                           # 下一步：网关二进制 + wait_for_master_schema + 上下线标记
```

共享 API 草案：

```rust
pub fn schema_version() -> i64;                    // 内嵌迁移的最大版本号

#[derive(Debug, Clone, Deserialize)]
pub struct DatabaseConfig { /* url, max_connections, connect/wait/lock timeouts */ }
impl DatabaseConfig {
    pub fn apply_env_overrides(&mut self);
}

pub struct Database { /* PgPool */ }
impl Database {
    pub async fn connect(cfg: &DatabaseConfig) -> anyhow::Result<Self>;   // 主控
    pub fn pool(&self) -> &sqlx::PgPool;
    pub async fn initialize_master(&self, cfg: &DatabaseConfig) -> anyhow::Result<()>;
    pub async fn verify_schema(&self) -> anyhow::Result<()>;
    pub async fn health_check(&self) -> anyhow::Result<()>;
}

pub async fn wait_for_master_schema(cfg: &DatabaseConfig) -> anyhow::Result<Database>;  // 网关
```

- 主控 `main`：加载配置 -> 初始化日志 -> `Database::connect` -> `initialize_master` -> `verify_schema`
  -> axum 提供 `GET /health`（返回 `{status, schema_version, database}`，DB 探测失败返回 503）。
- 网关（下一步）：加载配置 -> 初始化日志 -> `wait_for_master_schema` -> 在 `gateways` 标记 online 并起心跳
  -> 代理链路；优雅退出时标记 offline。

## 9. 测试与验证

- **单元测试（不依赖数据库，CI 必跑）**：
  - 内嵌迁移非空、最大版本号为正、`0001` 末尾确实调用 `wg_mark_schema_version(1)`；
  - 迁移包含 `CREATE EXTENSION IF NOT EXISTS uint128` / `btree_gin`，且 `ports`/`body_length`/`status` 使用 uint 类型；
  - `access_response_logs.status` 没有 RFC 范围 CHECK（上游可能返回非标准状态码）；
  - 每张表 DDL 都包含 `created_at`；可更新表都有 `updated_at` 触发器；
  - 访问日志表都是分区表、主键含分区键、无外键；
  - `gateways`、`access_stats_second`、`access_stats_minute`、`access_stats_daily`、`stats_state` 存在；
  - 迁移 0003 含水位列与分区回收函数，且以 `wg_mark_schema_version(3)` 结尾；
  - `DatabaseConfig` 默认值（网关等待 300s、锁超时 60s）与 URL 密码脱敏。
- **集成测试（设置 `DATABASE_URL` 才跑，否则自动跳过）**：
  1. 连接服务器，创建临时数据库 `webgateway_it_<随机>`（用完 `DROP DATABASE ... WITH (FORCE)`，beta 库不受影响）；
  2. 未初始化时网关等待：`wait_timeout_secs=2` 以 `timed out` 失败；
  3. 主控 `initialize_master`：`schema_state.version` 等于当前迁移最大版本（当前 5），
     扩展 `uint128`/`btree_gin` 已安装，`gateways` 表存在，四张日志表的周分区已预建；
  4. 再次执行 `initialize_master`：幂等（sqlx 跳过已应用迁移）；
  5. 网关等待：立即成功并通过 `verify_schema`；
  6. 清理临时库。
  7. `crates/shared/tests/stats_rollup.rs`：回拨水位 3 天 → 分钟/日级回填、最近 10s 进秒表、
     重复执行不重复计数、8 周前分区整周 DROP、当前周 + 未来 3 周存在。
- **人工验证**：在 `webgateway_beta_v2` 上启动主控完成建表，请求 `GET /health`，并用只读 SQL 核对表、触发器、分区。
- **CI**：Rust job 已随 workspace 落地自动启用；GitHub Runner 访问不到内网 beta 库，集成测试因无
  `DATABASE_URL` 自动跳过，CI 只跑 fmt / clippy / 单元测试 / 构建。

## 10. 实施状态

**已完成**：workspace（`shared` + `dashboard` + `gateway`）、完整 schema 迁移、主控迁移与就绪标记、
`verify_schema`、dashboard `/health`、单元测试 + 集成测试（临时库）；
网关二进制（等待就绪、`gateways` 上下线与心跳、HTTP/HTTPS 反向代理、站点级 headers、节点 header）；
配置同步改为 LISTEN/NOTIFY（迁移 0002 覆盖 DELETE）+ 水位校验，已移除 10s 全量轮询。

**下一步**：
1. 访问日志写入与秒级/分钟级/日级汇总、保留期清理；
2. 网关缓存（主控下发）、IP/UA 拦截；
3. 网关后端协议扩展：WebSocket / gRPC / HTTP/3 / 特殊 RFC；
4. CRUD/仓储层、REST API、认证、ACME、前端对接。

## 11. 决策记录（2026-11-18）

- 不迁移 v1 数据，v2 从空库开始。
- 先实现主控，再实现网关。
- `access_response_logs.status` 不加 RFC 范围 CHECK。
- 使用内部镜像的 `uint128` / `btree_gin` 扩展（主控迁移里 `CREATE EXTENSION IF NOT EXISTS`）。
- 网关上下线写入 `gateways` 表，在线判定叠加心跳新鲜度（中等优先级）。
- 统计粒度：最近 1 小时秒级，超过 1 小时归并到分钟级，日级汇总。
- 迁移使用 sqlx 迁移文件，主控独占执行；网关无 DDL 路径。
- 网关等待默认 300s（`DATABASE_WAIT_TIMEOUT=0` 表示无限），版本高于期望时 WARN 继续。
- 主控连接失败即退出，不在主控内无限重试。
- 配置同步改为 LISTEN/NOTIFY（迁移 0002 增加 DELETE 通知）+ 水位对比，不再每 10s 全量同步。
- 代理请求头下沉到 `websites.config.headers`（set/remove + `$remote_addr`/`$scheme`/`$host` 占位符），
  未配置时使用默认四个头；逐跳头始终剔除。
- `X-WebGateway-Node` 格式为 `<country-city>-<fnv1a64(location+machine_id) 16 hex>`，例如 `cn-shanghai-18411588dcb04048`。
- 网关后续支持 WebSocket / gRPC / HTTP/3 / 特殊 RFC（分阶段实现）。
- 日志/错误输出使用英文（AGENT.md）。
