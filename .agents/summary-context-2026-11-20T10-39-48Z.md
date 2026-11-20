# Context Summary — 2026-11-20T10:39:48Z

## 最终状态
- 分支 `feat/stats-accuracy`（HEAD 73ec94b，文档收尾），分支栈：
  `feat/health-checks` a5aee76 → `feat/h3-streaming` 30ac6b0 → `feat/cache-vary` 88b0191 →
  `feat/stats-accuracy` b22c12b → 73ec94b（最终清单文档）。
- 四个功能 head CI 全绿；最终文档提交 73ec94b 的 CI 见最终回复。
- schema v6（无新迁移）；beta DB 已清空（users/websites/certificates/gateways/logs/stats = 0），
  端口全部释放；仅 AGENT.md（用户未提交编辑）与 RECOVER*.md 未跟踪/未提交。
- 目标（goal-77f95d63）本轮收尾；代码层面已具备生产部署条件，剩余项为用户决策/外部接入。

## 本轮（第 6 轮）完成
- 叠加功能 smoke（最终证据）：
  - 加密私钥证书：dashboard 用 SECRET_ENCRYPTION_KEY 上传 → gateway 同密钥解密 → HTTPS 200；
  - h3：64 KiB 响应（GATEWAY_MAX_RESPONSE_BODY=1024）→ 200 + 65536 字节；
  - 缓存：MISS→HIT；健康检查：死后端 18081 被摘除，请求全部走 18080；
  - 面板静态托管：`/` 200 + 面板 CSP；`/api/summary?tz_offset_minutes=480` 200；
  - 指标：dashboard_database_up=1、websites_total=1；gateway http3_requests=1、cache hits/misses、
    health_check_failure、backends_unhealthy=1；
  - SIGTERM：慢 h3 请求在 drain 中返回 200/7 字节，gateway exit 0 并 flush/offline；dashboard exit 0。
- 文档收尾：
  - `deployment.md` 第 9 节限制表按现状更新，新增第 12 节“生产就绪状态”（9 项修复 + 提交 + CI + smoke）；
  - `issue.md` 第 5 节状态、第 6 节优先级（B-21 需用户决策）、新增第 7 节生产就绪汇总；
  - `changelog.md` 最终 6 批次收尾记录。
- 最终测试：gateway 53、dashboard 26、shared 19 + 3 集成；fmt/clippy 通过；
  前端 format/lint/type-check/build 通过。

## 生产就绪结论
- 可部署：单 dashboard 实例 + TLS 反向代理（或 DASHBOARD_STATIC_DIR 直接托管面板）、
  dashboard/所有 gateway 同一 `SECRET_ENCRYPTION_KEY`、按 `deployment.md` 第 8 节检查表上线。
- 已收敛：Cookie 会话/安全头/错误脱敏、静态托管/SIGTERM、密钥加密、Prometheus 指标、
  每 IP 限流、上游健康检查、h3 流式+drain、缓存 Vary/HEAD/304、统计 distinct/保留期/时区。
- 剩余（详见 deployment.md 第 9 节 / issue.md 第 7 节）：
  - **B-21 分支合并**（用户决策，唯一 P0 流程项）；
  - P2：refresh token/每用户 API Key/服务令牌过期、跨节点健康状态/熔断、WAF/CC、
    h3 缓存与请求体流式、缓存 single-flight/IMS/持久化、CI DB E2E、面板分页/版本；
  - P3：统计 sizes 精度、依赖审计。

## 关键事实 / gotchas（交接）
- `.tools/pq.py` 从 DEFAULT_URL 取 beta DB；不要打印凭据。
- `.venv` 的 aioquic 1.8：SNI 放 `QuicConfiguration(..., server_name=...)`，`connect()` 不接受。
- 周分区保留期最低 7 天（SQL 函数强制）；`STATS_RAW_RETENTION_DAYS` 低于 7 会夹紧并 WARN。
- dashboard summary 的“今日”扫描本地日原始日志（保留期内），不要误以为它读聚合表。
- 曾误用 `git reset --hard` 丢掉 AGENT.md 用户编辑（已恢复）；后续移动提交先建分支，勿对工作区 reset。
- 分支栈是逐层叠加的；合并/发布前需用户确认 B-21 方向。
