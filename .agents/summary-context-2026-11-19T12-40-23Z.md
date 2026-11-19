# Context Summary — 2026-11-19T12-40-23Z

## 当前状态
- 分支 `feat/stats-rollup` 已推送（统计聚合，CI 全绿）。
- 本轮新增网关缓存，待提交（将新建 `feat/gateway-cache`）。
- beta 库 schema version 2；测试数据已清理。

## 本轮完成
- `gateway/src/cache.rs`：64 MiB 内存 LRU；TTL/类型白名单/单条上限；配置同步清空。
- proxy：缓存查询在选后端之前；MISS 收集响应后写入；HIT 直接返回；ETag/If-None-Match → 304；Age/HIT/MISS 头。
- 请求带 Authorization/Cookie 跳过；响应含 Set-Cookie/Vary/no-store|private|no-cache 不缓存。
- E2E：MISS count=1 → HIT count=1 → 304 → SQL 更新触发同步后 MISS count=2。
- 网关单测 32 个。

## 未实现
- IP/UA 拦截、上游 TLS、h2 extended CONNECT、HTTP/3 流式/同连接并发。
- 缓存持久化/多级、stale-while-revalidate、Vary、命中率指标。

## 关键事实
- 缓存为进程内，重启清空；全局 64 MiB，LRU。
- 配置同步会清空全部缓存（非按站点）。
- 本机工具链 `.tools/`；beta 库连接串不进仓库。
