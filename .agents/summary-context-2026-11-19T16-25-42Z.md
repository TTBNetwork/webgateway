# Context Summary — 2026-11-19T16-25-42Z

## 当前状态
- 分支 `feat/dashboard-panel` 已推送（只读面板，CI 全绿）。
- 本轮新增 TOTP/JWT 认证 + 缓存默认值调整，待提交（将新建 `feat/dashboard-auth`）。
- beta 库测试数据已清理（users/websites/logs/stats = 0）。

## 本轮完成
- 主控 `auth.rs`：空 users 表创建默认 admin（TOTP secret 日志一次）；TOTP 校验（含 users_client_secrets）；
  每用户 JWT secret、HS256、默认 7 天；静态 DASHBOARD_API_TOKEN 作为服务令牌。
- `POST /api/auth/login` 公开；其余 /api 走 JWT/服务令牌中间件；`/api/auth/me` 返回当前用户。
- 面板登录改为用户名 + TOTP；JWT/用户存 localStorage；设置页显示账号/角色；退出清本地会话。
- 缓存默认 64 MiB → 16 MiB，新增 gateway 配置/环境变量。
- 验证：admin + TOTP 2026-11-19 登录 → JWT → /summary；dashboard 2 单测；前端 build/lint。

## 未实现
- 用户管理/RBAC/JWT 撤销/刷新、TOTP 重放保护/登录限速、服务令牌轮换。
- 面板 CRUD、ACME/DNS、静态托管；gRPC/Nezha 与 WS/MCSManager 联调未做。
- issue.md 中 P0/P1（分区续建、统计补算、WS TLS、h3 限制、优雅停机、日志可靠性）仍未修。

## 关键事实
- 认证后所有 /api 都需要 JWT；没有旧的“未配置 token 即开放”模式。
- 默认 admin 只在 users 为空时创建；丢失 TOTP secret 需手工重置数据库。
- jsonwebtoken 12 必须启用 rust_crypto（或 aws_lc）feature，否则运行期 panic。
- 本机工具链 `.tools/`；beta 库连接串不进仓库。
