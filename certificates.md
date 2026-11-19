# 证书管理：ACME 自动签发 / 手动上传

> 主控负责证书的创建、续期和存储；网关只读取 `certificates` 表并通过 SNI 加载。
> 支持两条路径：
>
> 1. **ACME 自动签发**：ZeroSSL（配置 EAB 时优先）→ Let's Encrypt 兜底，DNS-01，目前实现腾讯云 DNSPod。
> 2. **手动上传**：粘贴 fullchain + private key，校验 SAN/私钥格式后入库。
>
> schema version 4；相关代码：`dashboard/backend/src/certificate.rs`、`dashboard/backend/src/router.rs`。

## 1. 数据模型（迁移 0004）

`certificates` 新增列：

| 列 | 说明 |
|---|---|
| `auto_renew` | TRUE = 自动签发/续期；手动上传为 FALSE |
| `dns_zone` | DNS-01 使用的父区域（例如 `atxa.top`） |
| `last_error` | 最近一次签发失败原因，成功或重新发起时清空 |

迁移文件：`crates/shared/migrations/0004_certificate_issuance.sql`。原有
`signing_started_at` 作为签发租约（10 分钟），多主控实例通过它 + `FOR UPDATE SKIP LOCKED` 避免重复签发。

ACME 账户私钥按 CA 持久化在 `configurations` 表：

```
key = acme_account:letsencrypt | acme_account:zerossl
value = { "key_pem": "-----BEGIN PRIVATE KEY-----..." }
```

这样重启/重试不会重复创建 Let's Encrypt 账户（newAccount 用同一把 key 会返回既有账户），
也不会消耗 new-account 速率限制。**该私钥与其它控制面 secret 一样是明文存储**，后续应改为加密/KMS。

## 2. API

所有接口需要 JWT 或 `DASHBOARD_API_TOKEN` 服务令牌。证书写接口要求 `user` 或 `admin`；
站点/用户/DNS provider 写接口要求 `admin`；`view` 只读。

### 2.1 手动上传

```http
POST /api/certificates
Authorization: Bearer <token>
Content-Type: application/json

{
  "name": "cert-test",
  "hostnames": ["cert-test.atxa.top"],
  "fullchain": "-----BEGIN CERTIFICATE-----...",
  "private_key": "-----BEGIN PRIVATE KEY-----..."
}
```

- 校验：PEM 证书可解析、私钥 PEM 可解析、证书至少有一个 DNS/IP SAN；
  `hostnames` 可省略（从 SAN 推导），若提供则必须被 SAN 覆盖（支持 `*.example.com` 单 label 通配）。
- `expires_at` 从证书 `notAfter` 提取。
- 响应：`{ "id": ..., "hostnames": [...], "expires_at": 1795..., "status": "valid" }`。
- 入库后 certificates 的 NOTIFY 触发器会让网关自动重新同步。

### 2.2 自动签发

```http
POST /api/certificates/issue
{
  "name": "acme-test",
  "hostnames": ["cert-test2.atxa.top"],
  "email": "admin@atxa.top",
  "dns_provider_id": null,
  "dns_zone": "atxa.top"
}
```

- 创建 `auto_renew = TRUE` 的 pending 行并立即触发；响应 `{ "id": ..., "status": "queued" }`。
- `dns_zone` 可省略：优先取 `dns_provider.domains[0]`，否则按“去掉最左 label”推导（`cert-test.atxa.top` → `atxa.top`）。
- 一个证书的所有 hostname 必须在同一个 zone 内；多 zone 证书当前拒绝。
- 重新签发：`POST /api/certificates/{id}/issue`（只允许 `auto_renew` 行）。
- 删除：`DELETE /api/certificates/{id}`。

### 2.3 DNS Provider

```http
GET  /api/dns-providers
POST /api/dns-providers
{
  "name": "atxa-top",
  "domains": ["atxa.top"],
  "kind": "tencent",
  "from_env": true
}
DELETE /api/dns-providers/{id}
```

- `from_env: true` 表示运行时从环境读取 Secrets，接口与数据库都不保存密钥。
- 也支持 `from_env: false` + `secret_id`/`secret_key`（会落库，接口不会回显）。
- 目前只实现 `tencent`（腾讯云 DNSPod / DNSPod v3 API）。

## 3. DNS 凭据与 `.secrets/dnsprovider`

密钥文件被 `.gitignore` 忽略，**不进入仓库、不会上传、日志不打印值**。文件格式是简单的
`NAME = value`（允许空格与引号；`export` 可选）：

```
TENCENT_KEY = <SecretId>
TENCENT_SECRET = <SecretKey>
# 可选：启用 ZeroSSL（优先于 Let's Encrypt）
ZEROSSL_EAB_KID = ...
ZEROSSL_EAB_HMAC_KEY = ...
```

应用方式（不读取/打印值）：

```bash
# 方式一：source 到当前 shell
. scripts/source-dnsprovider.sh

# 方式二：直接启动主控（内部自动 source）
DATABASE_URL=... scripts/run-dashboard.sh
```

主控识别的腾讯云变量对（按顺序匹配）：

```
TENCENT_KEY / TENCENT_SECRET
TENCENT_KEY / TENCENT_SECRET_KEY
TENCENT_KEY / TENCENT_SECRETKEY
TENCENTCLOUD_SECRET_ID / TENCENTCLOUD_SECRET_KEY
TENCENT_SECRET_ID / TENCENT_SECRET_KEY
TENCENT_DNSPOD_SECRET_ID / TENCENT_DNSPOD_SECRET_KEY
DNSPOD_SECRET_ID / DNSPOD_SECRET_KEY
Tencent_SecretId / Tencent_SecretKey
```

其它环境变量：

| 变量 | 说明 |
|---|---|
| `ACME_EMAIL` | 证书未填 email 时的默认 ACME 联系邮箱 |
| `ZEROSSL_EAB_KID` / `ZEROSSL_EAB_HMAC_KEY` | ZeroSSL EAB；不配置则跳过 ZeroSSL，直接 Let's Encrypt |
| `ACME_DNS_PROPAGATION_SECS` | DNS-01 提交前等待秒数，默认 15；网络慢可调大 |
| `HTTPS_PROXY` / `HTTP_PROXY` / `NO_PROXY` | reqwest 默认读取，用于 ACME / DNSPod / 网络不可达时走代理 |

## 4. 签发流程

1. 调度器每 60s 认领：
   - `fullchain IS NULL`（pending），或
   - `auto_renew` 且 `expires_at < now() + 30 days`（续期）；
   - `signing_started_at` 为空或超过 10 分钟租约；`FOR UPDATE SKIP LOCKED` 防止多实例重复。
2. 按 CA 顺序尝试：
   - 配了 `ZEROSSL_EAB_*` → ZeroSSL（EAB JWS，HMAC-SHA256）；
   - 始终加入 Let's Encrypt 兜底；
   - 每个 CA 使用独立持久化的 account key。
3. DNS-01：
   - TXT 名 = `_acme-challenge.<identifier>`（不是父 zone）；
     例如 `cert-test2.atxa.top` → `_acme-challenge.cert-test2.atxa.top`；
   - 腾讯云 API 创建 TXT → 等待 `ACME_DNS_PROPAGATION_SECS` → 提交 challenge；
   - 成功后删除 TXT；失败路径也会清理（多 hostname 会逐个记录、逐个删除）。
4. 成功：更新 `fullchain`、`private_key`、`expires_at`，清空租约与 `last_error`；
   失败：写 `last_error`，保留旧证书（续期场景），约 10 分钟后重试。

## 5. 实测（2026-11-19，beta 库 + 测试域名 `cert-test2.atxa.top`）

- 手动上传：经 API 上传自签测试证书 → DB 行 → 启动网关 → SNI 命中
  （`openssl s_client -servername cert-test2.atxa.top` 返回上传的 CN/SAN）。
- ACME：
  - ZeroSSL 未配置 EAB，按设计跳过；
  - Let's Encrypt 账户注册成功并持久化，DNS-01 TXT 创建/删除成功；
  - 订单 `ready` → 最终 `valid`，签发 90 天证书（issuer `C=US, O=Let's Encrypt, CN=YE1`，
    `notAfter=Feb 17 2027`）；
  - 网关通过 NOTIFY 加载新证书，`https://cert-test2.atxa.top:18443` 返回 200，证书由网关提供；
  - TXT 记录在成功后自动删除。
- 失败过的中间版本（仅记录用于排障）：JWS 必须是 JSON 对象、Let's Encrypt 只接受 RS/ES
  算法（ES256）、CSR 不能带 rcgen 默认 CN、DNS-01 记录名必须是完整 identifier。

## 6. 限制

- DNS provider 只有腾讯云 DNSPod；没有 HTTP-01 / TLS-ALPN-01 / 其他 DNS 服务商。
- 一个证书只能覆盖一个 DNS zone；多 zone 需拆成多张证书。
- ZeroSSL 必须提供 EAB；否则自动回退 Let's Encrypt。
- 通配符证书（`*.example.com`）代码按 identifier 生成 `_acme-challenge.example.com`，但尚未实测。
- ACME account 私钥、证书私钥均为明文入库；后续需要加密存储与轮换。
- ACME HTTP 请求 60s 轮询、失败约 10 分钟后重试；没有更细的退避/告警。
- 面板按钮使用当前登录 JWT；证书写接口按 `user`/`admin` 授权（`view` 只读）。

## 7. vendored acmex（0.8.0）

发布版 acmex 0.8.0 在本项目的实际使用中不可用，`vendor/acmex` 保留同版本并修复：

1. 去掉硬编码的 aws-lc-rs `fips` feature（与 v1 相同的构建修复）。
2. JWS 输出改为 RFC 7515 JSON 对象（原先是 `header.payload.signature` 字符串）。
3. 账户 key 改为 ECDSA P-256 + `ES256`（Let's Encrypt 不支持 EdDSA），JWK 使用 EC/P-256。
4. ECDSA DER 签名转为 JWS 需要的 raw `r || s`。
5. 账户注册响应解析：Let's Encrypt 的 newAccount body 不含 `id`/可选 `contact`，放宽字段。
6. POST-as-GET（获取 order/authorization/证书）使用空 payload，而不是 `{}`。
7. CSR 的 subject 置空（rcgen 默认 CN `rcgen self signed cert` 会被 CA 拒绝）。
8. EAB（RFC 8555 §7.3.4）支持。
9. DNS-01 TXT 名称按 identifier 生成；多记录清理；DNS 传播等待。
10. 失败路径清理 challenge 记录（成功路径原本就有清理）。

上游版本升级时优先替换 vendor，并逐条对照上面这些补丁。许可证：MIT OR Apache-2.0（见 vendor/acmex）。
