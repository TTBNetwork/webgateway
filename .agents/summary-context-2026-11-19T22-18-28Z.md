# Context Summary — 2026-11-19T22-18-28Z

## 当前状态
- 分支 `feat/certificates`（叠加在 `fix/gateway-websocket-tls` 之后），本轮已完成代码 + 面板 + 文档。
- beta 库 schema **v4**；证书/站点/网关/用户/日志/统计测试数据已清理（全部 0 行），
  `configurations` 中测试用的 `acme_account:*` 已删除，`stats_state.last_aggregated_at = NULL`。
- 测试 DNS：`_acme-challenge.atxa.top` 遗留 TXT 已通过 DNSPod API 删除（record 2454584254）；
  测试证书 `cert-test.atxa.top` / `cert-test2.atxa.top` 已从库中删除。
- 所有测试进程（dashboard / gateway / MCS / Nezha）已停止。

## 本轮完成：主控证书管理

### 数据 / 后端
- 迁移 `crates/shared/migrations/0004_certificate_issuance.sql`：`auto_renew`、`dns_zone`、`last_error`、
  续期 partial index；schema v4；`verify_schema` 增加对应列。
- `dashboard/backend/src/certificate.rs`：
  - 手动上传：PEM 解析（x509-parser）、私钥校验（rustls-pemfile）、SAN/到期时间、hostname 覆盖校验；
  - ACME：60s 调度、`signing_started_at` 10 分钟租约 + `FOR UPDATE SKIP LOCKED`、到期前 30 天续期；
  - ZeroSSL（EAB 环境变量存在时优先）→ Let's Encrypt；DNS-01 腾讯云 DNSPod；
  - ACME account key 按 CA 持久化到 `configurations`（`acme_account:<ca>`）；
  - 失败写 `last_error`，约 10 分钟后重试；失败路径清理 DNS TXT；
  - API：上传 / 自动签发 / 重新签发 / 删除证书，DNS provider 列表/创建/删除（不回显密钥）。
- 面板：证书页自动/手动表单、重新签发、删除、pending/error 展示；中英 i18n。

### 凭据
- `scripts/source-dnsprovider.sh` 解析 `.secrets/dnsprovider` 的 `NAME = value`（空格/引号/export 均可），
  `scripts/run-dashboard.sh` 直接启动主控；值从不打印；`.secrets/` 在 .gitignore。
- 识别 `TENCENT_KEY/TENCENT_SECRET`（实际文件格式）等别名；支持 ZeroSSL EAB、ACME_EMAIL、
  HTTPS_PROXY、ACME_DNS_PROPAGATION_SECS。

### vendored acmex 0.8.0（vendor/acmex，MIT OR Apache-2.0）
发布版不可用于真实 CA，已修复：FIPS feature、JWS JSON 对象、ES256/EC P-256 账户 key、
ECDSA DER→raw、newAccount 响应解析、POST-as-GET 空 payload、CSR 空 subject、EAB、
DNS-01 按 identifier 生成 TXT、传播等待、多记录/失败路径清理。

### 实测
- 手动上传：API → DB → 网关 SNI 返回上传证书（CN/SAN 验证）。
- Let's Encrypt DNS-01（`cert-test2.atxa.top`）：账户复用、TXT 创建/删除、订单 ready→valid、
  签发 90 天证书（issuer `CN=YE1`，notAfter 2027-02-17）；网关 NOTIFY 加载，
  `https://cert-test2.atxa.top:18443` 返回 200。
- ZeroSSL 未配置 EAB，按设计跳过并回退 LE。

## 测试
- 网关 34、dashboard 5、shared 12 + 3 集成；`cargo fmt` / `clippy -D warnings` 通过。
- 前端 `pnpm format:check` / `lint:check` / `type-check` / `build` 通过。
- 推送后需确认 GitHub Actions 三个分支（含本分支）运行结果。

## 关键事实
- `.secrets/dnsprovider` 使用 `NAME = value`（带空格）格式，不能直接 `source`；必须用
  `scripts/source-dnsprovider.sh` 的解析器。真实环境变量对为 `TENCENT_KEY/TENCENT_SECRET`。
- Let's Encrypt 当前只接受 RS256/ES256/ES384/ES512；acmex 已改为 ES256/EC P-256。
- ACME 账户 key 与证书私钥均为明文入库；后续需加密。
- 测试域名按 AGENT.md 使用 `atxa.top` 前缀；本轮使用 `cert-test.atxa.top`、`cert-test2.atxa.top`。

## 下一步
1. 站点/网关/用户 CRUD + RBAC 细分（证书已可写，仍未按角色授权）。
2. ZeroSSL EAB 实测；通配符证书实测；多 DNS provider / HTTP-01。
3. ACME account key / 证书私钥加密存储与轮换。
4. B-04/B-05/B-06、metrics、分支合并（issue.md 优先级）。
