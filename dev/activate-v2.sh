#!/usr/bin/env bash
#
# 激活 v2 分区表：**不搬数据**，把逻辑名（access_request_logs 等）交给 v2 分区表，
# 原 v1 表改名成 `access_*_v1` **永久保留**（不会被自动回收）。
#
# 用法（部署目录下执行）：
#   ./activate-v2.sh            # 执行激活（会自动停网关 → 激活 → 再起网关）
#   ./activate-v2.sh --check    # 只看当前状态，不改任何东西
#
# 为什么激活要停网关：换名是 `ALTER TABLE ... RENAME`，对持有旧表引用的写入方
# 不友好（网关正在往 v1 写日志）。停几秒比"边写边换名"安全得多。
#
# 什么时候需要它：
#   * 老库（逻辑名上还是 v1 普通表）想切到 v2 → 需要跑一次；
#   * 全新库不需要（启动迁移会自动把逻辑名交给 v2）；
#   * 已经切过的库再跑是安全的（幂等，会直接返回）。
#
# 回滚（激活后想退回去，秒级、无损）：
#   把 access_* 与 access_*_v1 的名字互换即可，见脚本末尾打印的 SQL。
set -euo pipefail

COMPOSE=${COMPOSE:-docker compose}
CHECK=0
[[ "${1:-}" == "--check" ]] && CHECK=1

say() { printf '\n\033[1m== %s\033[0m\n' "$*"; }
die() { printf '\n\033[31m✗ %s\033[0m\n' "$*" >&2; exit 1; }

[[ -f docker-compose.yml ]] || die "请在部署目录执行（找不到 docker-compose.yml）"
PSQL="$COMPOSE exec -T postgres psql -U ${POSTGRES_USER:-root} -d ${POSTGRES_DB:-postgres} -tAc"

status() {
  $PSQL "SELECT relkind FROM pg_class WHERE relname='access_request_logs'" | tr -d '[:space:]'
}

print_state() {
  say "当前状态"
  $COMPOSE exec -T postgres psql -U "${POSTGRES_USER:-root}" -d "${POSTGRES_DB:-postgres}" -c "
    SELECT c.relname, c.relkind,
           CASE c.relkind WHEN 'p' THEN '分区表(v2 已激活)'
                          WHEN 'r' THEN '普通表(v1 形态)'
                          ELSE c.relkind::text END AS 说明
      FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
     WHERE n.nspname='public' AND c.relname IN
           ('access_request_logs','access_request_logs_v1',
            'access_response_logs','access_response_logs_v1',
            'access_request_size_logs','access_request_size_logs_v1',
            'access_response_size_logs','access_response_size_logs_v1')
     ORDER BY c.relname" 2>&1 | sed 's/^/  /'
  $PSQL "SELECT 'phase=' || COALESCE((SELECT phase FROM access_log_v2_migration WHERE id=1),'(无进度表)')" | sed 's/^/  /'
  $PSQL "SELECT 'v1 行数: ' || COALESCE((SELECT count(*)::text FROM access_request_logs_v1),'(无)')" 2>/dev/null | sed 's/^/  /' || true
}

print_state
[[ $CHECK == 1 ]] && exit 0

KIND=$(status)
if [[ "$KIND" == "p" ]]; then
  say "已经是 v2（逻辑名是分区表）—— 无需操作"
  exit 0
fi
[[ "$KIND" == "r" ]] || die "access_request_logs 既不是普通表也不是分区表（relkind=$KIND），请人工确认"

say "确认要激活（这是**结构切换**，v1 会改名成 *_v1 保留，数据不删）"
read -r -p "输入 yes 继续: " ANS
[[ "$ANS" == "yes" ]] || die "已取消"

say "停掉网关（换名是 ALTER TABLE ... RENAME，激活期间不应有进程在写 v1）"
$COMPOSE stop gateway

say "执行激活（后端容器内一次性命令，不搬数据）"
$COMPOSE exec -T -e ACCESS_LOG_V2_ACTIVATE=1 dashboard-backend \
  /opt/webgateway/dashboard --activate-v2-keep-v1

say "重新启动网关"
$COMPOSE start gateway
sleep 3

say "校验"
KIND=$(status)
[[ "$KIND" == "p" ]] || die "激活后 access_request_logs 仍不是分区表，请检查日志"
V1ROWS=$($PSQL "SELECT count(*) FROM access_request_logs_v1")
echo "  access_request_logs 已是分区表 ✓"
echo "  v1 历史数据保留在 access_request_logs_v1：$V1ROWS 行 ✓"
$COMPOSE exec -T postgres psql -U "${POSTGRES_USER:-root}" -d "${POSTGRES_DB:-postgres}" -c "
  SELECT p.relname AS 父表, count(*) AS 分区数
    FROM pg_inherits i JOIN pg_class p ON p.oid=i.inhparent
   WHERE p.relkind='p' AND p.relname LIKE 'access%'
   GROUP BY p.relname ORDER BY p.relname" 2>&1 | sed 's/^/  /'

cat <<'EOF'

✓ 激活完成：新日志写入按周分区，旧数据保留在 *_v1。

后续（可选）：
  * 把 v1 历史数据搬进 v2（可续跑、搬完不删 v1；mnt 在后端容器里）：
      docker compose exec dashboard-backend mnt migrate-v2
  * 确认无误、想回收那 5~6 GB 空间时再手工删：
      DROP TABLE access_response_size_logs_v1;
      DROP TABLE access_request_size_logs_v1;
      DROP TABLE access_response_logs_v1;
      DROP TABLE access_request_logs_v1;

回滚（秒级、无损）：
  BEGIN;
  ALTER TABLE access_request_logs        RENAME TO access_v2_req_logs;
  ALTER TABLE access_request_logs_v1     RENAME TO access_request_logs;
  ALTER TABLE access_response_logs       RENAME TO access_v2_resp_logs;
  ALTER TABLE access_response_logs_v1    RENAME TO access_response_logs;
  ALTER TABLE access_request_size_logs   RENAME TO access_v2_req_size_logs;
  ALTER TABLE access_request_size_logs_v1 RENAME TO access_request_size_logs;
  ALTER TABLE access_response_size_logs  RENAME TO access_v2_resp_size_logs;
  ALTER TABLE access_response_size_logs_v1 RENAME TO access_response_size_logs;
  COMMIT;
EOF
