# Context Summary — 2026-11-20T05:18:15Z

## 当前状态
- 分支 `feat/production-deploy`（叠加在 `feat/dashboard-session-security` 之后）。
- HEAD: 27b92f6（Cargo.lock）；功能提交 ac74a83（面板托管/停机）与 340d3c4（密钥加密）。
- 已 push 到 origin，CI 跟踪 27b92f6（run 见 final 回复）。
- schema 仍为 **v6**（本轮无迁移）；E2E 数据已清理（users/websites/certificates/gateways/dns_providers/logs/stats = 0），
  测试进程与端口已释放。

## 本轮完成（生产部署批次）

### 面板静态托管 + 停机
- `DASHBOARD_STATIC_DIR`（`[server].static_dir`）：dashboard 直接托管 `dashboard/frontend/dist`；
  `/api`、`/health` 优先，其余 SPA 回退 index.html；未知 `/api/*` 返回 JSON 404。
- 静态安全：拒绝 `..`/反斜杠/NUL；扩展名 MIME；`/assets/*` immutable，index.html no-store；
  HTML 用 `default-src 'self'` 面板 CSP，JSON 仍 `default-src 'none'`。
- dashboard 处理 SIGTERM 与 Ctrl-C；drain 上限 30s。
- **修复 E2E 发现的 bug**：30s 超时原先包住整个 serve 生命周期，启动 30s 后无信号也会退出；
  现在用 oneshot 通知 + 只对 drain 计时（见 dashboard/backend/src/main.rs）。
- 新增 `deployment.md`：拓扑、构建、DB 备份/迁移、环境变量、nginx/systemd、上线检查表、生产限制。

### 密钥静态加密（B-31/P-13）
- `shared::crypto`：AES-256-GCM（ring），格式 `enc:v1:<base64url(nonce||ct)>`；
  `SECRET_ENCRYPTION_KEY`（64 hex 或 base64 32B）+ `SECRET_ENCRYPTION_KEY_PREVIOUS`（仅解密）。
  未配置时明文并 WARN；`open()` 遇到密文但无密钥会报错，避免把密文当明文用。
- 加密字段：users.totp_secret/jwt_secret、users_client_secrets.secret、certificates.private_key、
  configurations 的 acme_account:* key_pem、dns_providers.provider 的 secret_id/secret_key；
  fullchain 不加密。
- dashboard 启动 `secrets::encrypt_legacy_secrets` 幂等加密历史明文；gateway 读取证书前解密，
  失败则跳过该证书并 WARN（回落 rcgen 自签）。
- `deployment.md` 4.1 记录上线顺序（先给 dashboard + 所有 gateway 配同一密钥，再重启 dashboard）、
  轮换方式与密钥丢失风险。

## 实测（beta DB）
- 先无密钥写入 2 用户/1 证书/1 DNS provider（明文）→ 带密钥重启：日志 `encrypted plaintext secrets count=4`，
  DB 全部 `enc:v1:`，DNS JSON 明文 secret_id 消失；admin 登录 200（TOTP/JWT 解密）。
- gateway 带密钥：`certificate_hosts=1`，`curl --resolve secure-test.atxa.top:18443` 200，
  证书 `CN=secure-test.atxa.top`（后端 python http.server）。
- gateway 无密钥：`cannot decrypt the certificate key`，证书数 0，SNI 回落 rcgen 自签。
- dashboard 无密钥访问已加密数据：登录 500 + error_id，日志 `failed to decrypt the TOTP secret`。
- 静态托管：`/` 200 HTML + 面板 CSP；`/websites` 回退；`/assets/*.js` immutable；
  `/assets/../../Cargo.toml` 不泄露；SIGTERM exit 0、~8ms 退出。

## 测试
- dashboard 23、gateway 37、shared 18 + 3 集成；`cargo fmt` / `clippy -D warnings` 通过。
- 前端 format/lint/type-check/build 通过（本轮只改文档，未变更前端源码）。
- 注意：CI 使用 `--locked`，新增依赖后必须提交 Cargo.lock（本轮已补 27b92f6）。

## 关键事实 / gotchas
- 启用加密后 gateway **必须**配置同一个 `SECRET_ENCRYPTION_KEY`，否则所有证书被跳过。
- 密钥轮换：旧密文可用 PREVIOUS 解密，但不会自动重加密；尚无自动轮换任务。
- 未设置密钥时行为与旧版本一致（明文），日志有 WARN；生产必须设置。
- 面板与 /api 必须同源；dashboard 自托管面板时不需要 nginx SPA 规则。
- E2E 登录 TOTP：`DASHBOARD_ADMIN_TOTP_SECRET` 需 base32（python `base64.b32decode` 要补 padding）。
- 测试用 dashboard 用 13000/13001，端口 18080 后端、18443 gateway；结束后清理。

## 下一步（生产清单剩余）
1. 可观测性：dashboard/gateway `/metrics`（缓存命中、日志丢弃、上游错误、统计延迟）+ 告警（B-25）。
2. HTTP/3：流式响应、缓存、停机 drain（B-04/P-01）。
3. 缓存正确性：Vary、single-flight、HEAD/304（B-09）。
4. 上游健康检查/失败摘除（P-10/U-17）、按 IP 限流（U-07）。
5. 统计：total_ips、保留期配置、UTC 日（B-08/B-14）。
6. refresh token / 每用户 API Key / 服务令牌过期（B-30 剩余）。
7. B-21 分支合并（需用户确认）；CI DB E2E（U-31）依赖 PG18+uint128 镜像。
