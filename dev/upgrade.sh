#!/usr/bin/env bash
#
# 生产升级：拉取新镜像 → 重建容器 → **自动校验**（不做任何数据变更）。
#
# 用法（在部署目录 /opt/webgateway 下执行）：
#   ./upgrade.sh              # 正常升级
#   ./upgrade.sh --no-backup  # 跳过备份（数据库很大、且已有近期备份时）
#
# 这个脚本**只做升级的必要动作**，刻意不包含：
#   * 不搬历史数据、不切 v2（那是单独一步，见 activate-v2.sh）；
#   * 不改 .env、不动数据库内容。
# 因此它可以随时重跑（幂等），失败也不会留下半迁移状态。
#
# 校验不通过时脚本会以非零退出码结束，并提示回滚方式（用镜像 sha 标签）。
set -euo pipefail

COMPOSE=${COMPOSE:-docker compose}
BACKUP=1
[[ "${1:-}" == "--no-backup" ]] && BACKUP=0

say() { printf '\n\033[1m== %s\033[0m\n' "$*"; }
die() { printf '\n\033[31m✗ %s\033[0m\n' "$*" >&2; exit 1; }

say "1/6 前置检查"
[[ -f docker-compose.yml ]] || die "当前目录没有 docker-compose.yml，请在部署目录执行"
[[ -f .env ]] || die "当前目录没有 .env（至少需要 SUBNET_PREFIX 与 POSTGRES_PASSWORD）"
$COMPOSE config -q || die "docker-compose.yml 校验失败"
PG=$($COMPOSE ps -q postgres || true)
[[ -n "$PG" ]] || die "找不到 postgres 容器（先 docker compose up -d 起一次）"
echo "部署目录: $(pwd)"
echo "当前镜像:"
$COMPOSE images | sed 's/^/  /'

if [[ $BACKUP == 1 ]]; then
  say "2/6 备份数据库（pg_dump -Fc，落在容器内 /tmp 并复制到宿主机 ./backups/）"
  mkdir -p backups
  STAMP=$(date +%Y%m%d-%H%M%S)
  OUT="backups/webgateway-$STAMP.dump"
  $COMPOSE exec -T postgres pg_dump -U "${POSTGRES_USER:-root}" -d "${POSTGRES_DB:-postgres}" -Fc -f "/tmp/$STAMP.dump"
  docker cp "webgateway-pg:/tmp/$STAMP.dump" "$OUT"
  $COMPOSE exec -T postgres rm -f "/tmp/$STAMP.dump"
  ls -lh "$OUT" | sed 's/^/  /'
  echo "  备份已保存：$OUT（回滚数据库时用它：pg_restore）"
else
  say "2/6 跳过备份（--no-backup）"
fi

say "3/6 停止并移除容器（**数据在 ./data/pg bind mount 里，不会丢**）"
$COMPOSE stop
$COMPOSE rm -f

say "4/6 拉取新镜像"
$COMPOSE pull

say "5/6 启动"
$COMPOSE up -d

say "6/6 校验"
# 6.1 容器都起来了
$COMPOSE ps | sed 's/^/  /'
RUNNING=$($COMPOSE ps --status running -q | wc -l | tr -d ' ')
[[ "$RUNNING" -ge 4 ]] || die "只有 $RUNNING 个容器在运行（应为 4）"

# 6.2 网关确实在监听 80
#
# 不在容器内探测：镜像基于 debian-slim，**没有 `ss` / `netstat`**，
# 之前那版会因此报错并把整个校验带崩。改为从宿主机探测；失败时打印网关
# 日志尾部供人工判断，而不是直接抛一句没有信息量的错误。
sleep 5
if (exec 3<>/dev/tcp/127.0.0.1/80) 2>/dev/null; then
  echo "  宿主机 80 端口在监听 ✓"
else
  echo "  ! 宿主机 80 端口探测失败，下面是网关日志尾部："
  $COMPOSE logs --tail=30 gateway 2>&1 | sed 's/^/    /' || true
  die "网关看起来没在监听 80"
fi

# 6.3 迁移/结构：v2 结构、汇总表都应存在
PSQL="$COMPOSE exec -T postgres psql -U ${POSTGRES_USER:-root} -d ${POSTGRES_DB:-postgres} -tAc"
V2=$($PSQL "SELECT count(*) FROM pg_class WHERE relkind='p' AND relname IN ('access_request_logs','access_response_logs','access_request_size_logs','access_response_size_logs')")
STATS=$($PSQL "SELECT to_regclass('public.access_stats_daily') IS NOT NULL")
echo "  逻辑名的 v2 分区表: $V2 / 4"
echo "  日汇总表存在: $STATS"
[[ "$V2" = "4" || "$V2" = "0" ]] || die "v2 分区表数量异常（$V2）"
if [[ "$V2" = "0" ]]; then
  echo "  说明：逻辑名上还是 v1 普通表 —— 尚未激活 v2（这是默认状态，不是错误）。"
  echo "        要切到 v2 请执行：./activate-v2.sh"
fi

# 6.4 日志里不应有启动期错误
if $COMPOSE logs --tail=200 gateway 2>&1 | grep -qiE "panicked|Failed to sync first config|Error: "; then
  echo "  ! 网关日志里有可疑行，请人工确认："
  $COMPOSE logs --tail=200 gateway 2>&1 | grep -iE "panicked|Failed to sync first config|Error: " | tail -5 | sed 's/^/    /'
  die "网关日志校验未通过"
fi

cat <<EOF

✓ 升级完成。

下一步（可选，互不依赖）：
  * 切到 v2 分区表（1 秒、不搬数据、v1 原样保留；会先停网关再起）：
      ./activate-v2.sh
  * 把 v1 历史数据搬进 v2（可断点续跑，搬完不删 v1；mnt 在后端容器里）：
      docker compose exec dashboard-backend mnt migrate-v2
  * 回填/对账面板日汇总：
      docker compose exec postgres psql -U \${POSTGRES_USER:-root} -d \${POSTGRES_DB:-postgres} \\
        -c "select day, sum(total_requests) from access_stats_daily group by day order by day desc limit 7"

回滚（如需）：容器用的是 :latest，回滚要指定旧镜像的 sha 标签：
  IMAGE_PREFIX=... docker compose up -d \\
    --no-deps gateway dashboard-backend   # 先把 .env 里的镜像标签改成旧的 sha
EOF
