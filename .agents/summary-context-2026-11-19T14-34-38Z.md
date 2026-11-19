# Context Summary — 2026-11-19T14-34-38Z

## 当前状态
- 分支 `feat/backend-weights` 已推送（权重 + HTTP/3 并发，CI 全绿）。
- 本轮新增控制面板（API + Vue UI），待提交（将新建 `feat/dashboard-panel`）。
- beta 库测试数据已清理（websites/certs/gateways/logs 均为 0）。

## 本轮完成
- 主控只读 API：summary / websites / certificates / gateways / logs；`DASHBOARD_API_TOKEN` Bearer 鉴权（未设置则开发模式开放）。
- Vue 3 面板：登录、概览、站点、证书、网关、日志、设置；自写 history router + fetch，无新增依赖。
- 视觉：深/浅色、中/英、响应式侧栏、统计卡片、SVG 流量图、表格/空状态/错误态。
- 验证：Vite 代理（401/200 + 真实数据）、`pnpm format:check`/`lint:check`/`build`、cargo fmt/clippy/test + integration 通过。

## 未实现
- CRUD（站点/证书/用户）、真正的用户登录/RBAC、ACME/DNS、审计。
- 面板静态托管（当前 Vite dev / 需 nginx fallback）；无前端单测/E2E。
- 面板数据只读；IP/UA 拦截按用户要求暂不做。

## 关键事实
- 前端未加 vue-router/ky（pnpm store 在沙箱只读，且避免 lock 变更）。
- Vite 代理 `/api`、`/health` → 127.0.0.1:3000；dashboard 需监听 3000。
- 登录令牌存 localStorage；服务端令牌比较为简单等值（MVP）。
- 本机工具链 `.tools/`；beta 库连接串不进仓库。
