# Context Summary — 2026-11-19T22-45-30Z

## 当前状态
- 分支 `feat/dashboard-rbac`（叠加在 `feat/certificates` 之后），已完成站点/用户 CRUD、RBAC、JWT 撤销。
- beta 库 schema **v5**；CRUD/RBAC 测试数据已清理（users/websites/certificates/gateways/logs/stats = 0），
  `configurations` 的测试 ACME key 已删除，`stats_state.last_aggregated_at = NULL`。
- 测试 dashboard 进程已停止；`.secrets/` 仍被 gitignore。

## 本轮完成

### 迁移
- `crates/shared/migrations/0005_user_token_version.sql`：`users.token_version`；schema v5。
- `verify_schema` / 单元测试 / `database.md` 已同步。

### 后端
- `dashboard/backend/src/auth.rs`：
  - JWT claims 加 `token_version`，认证时与 DB 比对；logout / 管理员 revoke / 角色变更 / TOTP 重置都 bump；
  - 用户管理函数：列表、创建（返回一次性 TOTP secret）、改角色、删除、重置 TOTP、撤销；
  - `is_admin`（admin/service）、`can_write_certificates`（admin/service/user）、角色校验；last-admin 保护。
- `router.rs`：路由分 read / certificate-write / admin 三组；
  - read：summary、websites、certificates、dns-providers、gateways、logs、auth/me、auth/logout；
  - cert-write：证书上传/签发/重新签发/删除（user 或 admin）；
  - admin：dns-providers 写、websites 写、gateways 更新/删除、users 全部（admin）；
  - 403 JSON：admin role required / certificate write permission required。
- `websites.rs`：站点 hosts/ports/backends/config 校验，创建/更新/删除；
  ports 通过 `$4::int[]::uint2[]` 写入；URL 仅 http/https。
- 新增依赖 `url = 2.5`。

### 前端
- 新增 `UsersView.vue`：用户列表、创建（显示一次性 TOTP secret）、角色下拉、重置 TOTP、撤销、删除；
  导航仅 admin 可见。
- `WebsitesView.vue`：新增/编辑/删除表单（hosts/ports/backends JSON/config JSON），仅 admin 显示；
  非 admin 只读。
- `AppLayout.vue`：users 导航/路由、异步 logout（先调 `POST /api/auth/logout` 再清本地）、footer schema v5。
- `api.ts`：用户/站点/网关写接口与类型；`AppIcon` 加 users 图标。
- i18n 中英：users.*、websites.* 新键。

### 验证（beta 库）
- 服务令牌创建站点（ports {18080}）与 view 用户；site PUT/DELETE 正常。
- view JWT：GET /api/websites 200；POST /api/websites、POST /api/certificates、GET/POST /api/users 均 403。
- revoke / logout 后旧 JWT 用 /auth/me 返回 401；角色变更 bump token_version；删除最后一个 admin 返回 400。
- `cargo test --workspace`：dashboard 8、gateway 34、shared 13 + 2/1 集成；clippy 通过。
- 前端 format/lint/type-check/build 通过。

## 关键事实
- 角色语义：admin 全部；user 可管理证书；view 只读；服务令牌（DASHBOARD_API_TOKEN）= admin。
- JWT 撤销是"每用户全局版本"：logout 会撤销该用户所有设备的 token。
- 新用户/重置 TOTP 的 secret 只在响应中出现一次，面板会显示但不再可查询。
- 站点写入会经 NOTIFY 让网关自动重载（含端口增删）。
- 网关列表仍只读；网关元数据更新/删除只有 API，面板未加按钮。

## 下一步（issue.md）
1. JWT refresh token / 每用户 API key、服务令牌轮换；TOTP 重放/限速（B-29）。
2. B-04 / B-05 / B-06：h3 限制、优雅停机、日志可靠性。
3. U-11/U-13、U-17/U-19/U-23；B-21 分支合并。
4. ZeroSSL EAB 实测、通配符证书、证书私钥加密。
