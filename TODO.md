# TODO

> 本文件由 AI 扫描代码生成，标记 `待确认` 的条目无法从代码中判断意图，需人工确认后再处理。

## 待定清单

### 数据面（gateway）

- [x] 实现 TLS SNI 解析（`crates/protocols/src/tls.rs`）
- [x] 实现按 SNI/Host 的站点路由与上游转发（`gateway/src/upstream.rs`）
- [x] 实现上游连接池（`gateway/src/upstream/connection.rs`）
- [x] 实现证书热加载 / 自动证书解析（`gateway/src/sync/cert.rs`）
- [x] 实现访问日志采集与落库（`gateway/src/access.rs`）
- [x] 支持 WebSocket 升级转发
- [ ] 实现 PROXY protocol 解析（`gateway/src/upstream/protocols.rs:26` 明确标注 `TODO: implement`，当前直接返回 `None`）
- [ ] 支持站点配置多后端负载均衡（`gateway/src/state.rs:22`：目前仅使用 `backends.first()`）
- [ ] 支持按 `match_path` 的路径级路由匹配（模型已有 `match_path` 字段，未见使用）
- [ ] 清理或合并已停用的 `gateway/src/proxy/`（`main.rs` 中 `pub mod proxy;` 被注释，与 `upstream` 功能重叠）
- [ ] 清理空文件：`gateway/src/dns.rs`、`gateway/src/foundation.rs`、`gateway/src/proxy/rules.rs`

### 控制面（dashboard）

- [x] 用户登录与 JWT 鉴权（`dashboard/backend/src/auth/jwt.rs`）
- [x] TOTP 绑定与验证（`dashboard/backend/src/auth/totp.rs`、`bind.rs`）
- [x] 站点 / 证书 / DNS 提供商 / 日志 / 访问统计 API
- [x] 前端登录、菜单、站点管理、统计图表
- [x] 访问统计地图与 QPS 图表（`echarts`）
- [ ] 证书申请流程补全（`dashboard/backend/src/certificate.rs:48` 待检查证书是否已在 PENDINGS；`router/certificate.rs:51` 待校验证书有效性）
- [ ] 决定 `dashboard/backend/src/ip/ip2region.rs` 的去留（`ip.rs` 中已注释停用）
- [ ] 统一错误响应与 HTTP 状态码（`dashboard/backend/src/response.rs` 待确认是否覆盖全部路由）
- [ ] 前端 `main.js` 与 `main.ts` 并存（`index.html` 实际引用 `/src/main.ts`），确认 `main.js` 是否可删除

### 共享库与工具

- [x] `shared`：数据库初始化、扩展创建、NOTIFY 触发器、模型定义
- [x] `simple_shared`：`ObjectId`、mnt 协议、zigzag varint 编解码
- [x] `dnsprovider`：DNSPod（腾讯云）实现
- [x] `mnt`：运维 CLI 客户端（AdminTOTP）
- [ ] 补全 favicon 抓取（`crates/shared/src/site/favicon.rs` 两处 `TODO: Implement favicon fetching`）
- [ ] 完成 `crates/acme`：`CertificateBuilder` 中残留 `println!` 调试输出，`public_key` / `fullchain_key` 字段未被使用
- [ ] `crates/acme/src/main.rs` 使用硬编码绝对路径 `include_str!("/develop_workspaces/...")`，与仓库无关，待删除或改造
- [ ] `crates/acme` 已加入 workspace members（未提交），确认是否保留

### 工程化 / 测试

- [ ] 建立自动化测试（当前 Rust 与前端均无任何测试）
  - [x] 已起步（2026-10-02 第三轮）：`gateway` 的 `strip_port` 单元测试 + `dashboard` 授权链路集成测试（含 handler 级 403/200 断言，未设置 `DATABASE_URL` 时自动跳过）
  - [ ] 仍需覆盖：访问日志刷盘顺序、连接池归还语义、前端组件
- [ ] 引入数据库迁移方案（目前仅 `assets/sqls/access_init.sql`，表结构依赖运行期 `CREATE EXTENSION` / 触发器初始化）
- [ ] CI 增加 `cargo fmt --check`、`cargo clippy`、前端 `eslint` / `vue-tsc` 检查（现有 workflow 只构建镜像）
- [ ] 补充根目录 `README.md`（当前缺失，仅有前端模板 README）
- [ ] 前端 `package.json` 缺少 `lint` / `typecheck` 脚本
- [ ] 确认 `tests/` 目录用途：名称像测试目录，实际存放证书与启动日志且已被 gitignore

### 缺陷修复（详见 [ISSUES.md](ISSUES.md)）

> 状态更新（2026-10-02 第三轮）：已完成 P0 止血 + 安全问题 + 前端放大器 + 角色权限模型。
> 详见 [CHANGELOG.md](CHANGELOG.md) 的「第三轮」条目。以下用 `[x]` 标记已修复项，
> 并在需要时补充残留说明；未勾选项为后续迭代内容。

P0 事故级：

- [x] P0-1 访问日志刷盘改为"先写库成功、再删内存"，失败批次留在内存重试（`gateway/src/access.rs` 已重写）
- [x] P0-2 批量 INSERT 按 ≤1000 行分块；批量 UPDATE 改 `FROM (VALUES ...)`（`crates/shared/src/database/access.rs`）
- [x] P0-3 QPS 查询改为直接过滤 `requested_at`（sargable，`EXPLAIN` 确认走索引）；视图去掉冗余 `ORDER BY`
- [x] P0-3（残留，第六轮部分完成）**保留期自动清理**已实现：控制面板「设置 → 数据保留」可配 90~3650 天（默认 180），gateway 每小时按批清理。**分区（按周 RANGE）方案已完成**（第十轮结构 + 第十一轮 v1→v2 后台自动迁移：搬迁、切换、整周 DROP 回收，零停机），见 [CHANGELOG.md](CHANGELOG.md) 第十/十一轮；生产执行待确认
- [x] 新增（第六轮）修复 `access_response_size_logs` 的 7 倍存储膨胀：同一响应的多个 body chunk 改为内存累加、每响应只落一行
- [ ] 新增（第六轮待办）控制面板配置 HTTP/HTTPS 代理以续签证书。**经核实无需改代码**：acmex 用 `reqwest::Client::builder()` 且未调 `.no_proxy()`，给 dashboard-backend 设 `HTTPS_PROXY` / `HTTP_PROXY` / `NO_PROXY` 环境变量即可。若要做成面板字段，需在 `certificate.rs` 构造 acmex 客户端时读取配置（acmex 未暴露代理入口，需改 vendor 副本）
- [x] P0-1（残留，第四轮修复）批量 INSERT 改为幂等（`ON CONFLICT DO NOTHING`），部分成功后整批重试不再永久毒化刷盘队列
- [x] P0-3（残留，第七轮）size 明细表改为**按 (请求, 秒) 聚合**：保留秒级颗粒度、消除按 chunk 的 7 倍膨胀
- [x] 第七轮：默认启动即自动迁移（`AutoMigrate`），控制面表纳入共享迁移，gateway/dashboard 谁先启动都一样
- [x] P0-14（残留，第四轮修复）证书认领增加 10 分钟过期回收；并修复认领查询漏选 `email` 列导致自动续签 100% 失败
- [x] P0-5（回归，第四轮修复）连接任务结束后不再把连接放回空闲池（否则后续请求永久挂起）
- [x] P0-4 刷盘循环改为**最小 1 秒间隔**，超过 3s 记录 WARN，消除自激
- [x] P0-5 只有响应体被完整读到 EOF 才允许归还连接池；`write(&[])` 假健康检查已删除（真实网关 + 8MB 响应中途断开验证 32/32 无串包）
- [x] P0-6 设置有限连接池上限（默认 256）+ 取许可 5s 超时
- [ ] P0-6（残留）连接由 hyper `Connection` future 持有最长 120s，**空闲复用率仍低**；真正 keep-alive 归还待做
- [x] P0-7 DDL 收敛到单一迁移入口 + 事务级 advisory lock + `Serve`/`Migrate`/`AutoMigrate` 启动模式；触发器单事务重建；恢复 10s 兜底全量同步
- [x] P0-8 站点/证书/监听器改为**对账式同步**（已用真实网关验证改域名、删站点、端口回收均即时生效）
- [x] P0-9 触发器改用 `clock_timestamp()`（并已改为对账式同步，不再依赖水位）
- [x] P0-10 TLS `handshake_len` 上限 64KB + 预读 10s 超时
- [x] P0-11 统计类请求 `retry: 0` + `timeout: 15000`，非 200 显式抛错以触发退避
- [x] P0-12 新增 `useVisiblePolling`，隐藏页面暂停轮询、恢复即刷新、失败指数退避
- [x] P0-13 证书续签判定改为 `expires_at < NOW() + '7 days'`，并新增 `idx_certificates_expires_at`
- [x] P0-14 证书签发改数据库侧 `FOR UPDATE SKIP LOCKED` 认领；`SigningGuard` 保证 panic 路径也释放；调度器句柄修正

P1 严重：

- [x] P1-1 移除 release 下 `access_map` 恒返回空的短路；新增 `(remote_addr, requested_at)` 索引
- [ ] P1-2 启用上游 TLS 与 HTTP/2（当前 `https://` 后端退化为明文 TCP）—— 本轮未做
- [ ] P1-3 解除连接级 300s / 120s 硬超时对 WebSocket、长连接、SSE 的截断 —— 本轮未做
- [x] P1-4 修正 JWT `exp` 与 `exp_at` 的单位不一致（统一为秒）
- [x] P1-5 修复上游路径拼接的越界 panic 与 base path 被丢弃问题
- [x] P1-6 证书通配符正则改为预编译
- [x] P1-7 不再持有连接池互斥锁做健康检查
- [x] P1-8 监听端口改为先 bind 成功再登记，bind 失败明确报错
- [x] P1-9 `/access/qps` 的 `count` 加 1..=300 校验
- [ ] P1-10 初始化 DDL 避免每次启动重放 —— 已由 P0-7 的启动模式拆分覆盖（服务进程不再执行 DDL）
- [x] P1-11 统计页日期切换加防抖 + `AbortController` + 序号守卫（前端）
- [ ] P1-12 日志分页 N+1 改为服务端批量接口 —— 本轮未做
- [x] P1-13 修复访问地图：保存响应数据、接通 watcher、透传 `in_days`
- [ ] P1-14 站点列表加分页 —— 本轮未做
- [x] P1-15 修复 `/auth/info` 越权读取（读他人需 admin）
- [x] P1-16 引入角色/权限模型（admin/user/view）+ `Authorizer` 提取器；`/auth/users` 限管理员

前端放大器：

- [x] P0-11 统计类 GET 显式 `retry: 0`
- [x] P0-12 轮询接入 `visibilitychange` 门控

### 顺带修复（审计报告之外发现）

- [x] Host 头端口未归一化导致**非 80/443 端口监听完全失效**（`Host: example.com:8080` 匹配不到只配 `example.com` 的站点）。已支持 `example.com:8080` / `[::1]:8080` 并补单元测试。
- [x] `certificate::init()` 返回真调度器 `JoinHandle`（原先 `abort()` 停不掉调度器）。


### 工程化 / 测试

- [ ] 建立自动化测试（当前 Rust 与前端均无任何测试）
  - [x] 已起步（2026-10-02 第三轮）：`gateway` 的 `strip_port` 单元测试 + `dashboard` 授权链路集成测试（含 handler 级 403/200 断言，未设置 `DATABASE_URL` 时自动跳过）
  - [ ] 仍需覆盖：访问日志刷盘顺序、连接池归还语义、前端组件
- [ ] 引入数据库迁移方案（目前仅 `assets/sqls/access_init.sql`，表结构依赖运行期 `CREATE EXTENSION` / 触发器初始化）
- [ ] CI 增加 `cargo fmt --check`、`cargo clippy`、前端 `eslint` / `vue-tsc` 检查（现有 workflow 只构建镜像）
- [ ] 补充根目录 `README.md`（当前缺失，仅有前端模板 README）
- [ ] 前端 `package.json` 缺少 `lint` / `typecheck` 脚本
- [ ] 确认 `tests/` 目录用途：名称像测试目录，实际存放证书与启动日志且已被 gitignore

## 待确认

- [ ] `crates/protocols` 未显式列入 `[workspace].members`，仅靠 `gateway` 的 path 依赖隐式纳入，是否需要显式声明？**待确认**
- [ ] `crates/protocols/src/proxyprotocol/v1.rs`、`v2.rs` 为空文件，是待实现还是废弃？**待确认**
- [ ] `gateway/src/dns.rs`、`gateway/src/foundation.rs` 为空文件，是预留模块还是残留？**待确认**
- [ ] 根目录 `.lh/` 目录（含大量 `*.json` 文件快照）用途不明，是否应保留在仓库？**待确认**
- [ ] `assets/error_pages/` 是否为网关当前使用的错误页，以及如何被网关加载？**待确认**
- [ ] `assets/ipdb/` 为空且被 gitignore，生产环境 IP 库的注入方式（构建期 or 运行时挂载）？**待确认**
- [ ] `dev/environment` 中写死了内网数据库地址，是否需要改为模板文件？**待确认**
- [ ] `dashboard/frontend/vited.config.ts` 疑似拼写错误或废弃文件，是否删除？**待确认**
- [ ] `Cargo.lock` 与多个 `Cargo.toml` 处于未提交状态，需要确认提交范围。**待确认**
- [ ] 项目版本号 `0.1.0` 与 CHANGELOG 的发布策略（tag 规则 `v*`），首个正式版本何时发布？**待确认**
- [ ] `gateway` 与 `dashboard` 的 `config.toml` 格式与示例文件缺失，是否需要提供样例？**待确认**
