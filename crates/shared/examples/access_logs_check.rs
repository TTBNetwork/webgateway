//! 访问日志**读写冒烟测试**（对着真实的库跑一遍仓库里的读写代码）。
//!
//! 用途：v1→v2 分区表切换之后，确认刷盘路径在**分区表**上仍然成立 ——
//! 这是最容易静默坏掉的地方（例如 `ON CONFLICT` 的冲突目标、size 表的按秒累加、
//! 父行守卫、以及 `get_*` 聚合查询）。
//!
//! ```bash
//! DATABASE_URL=... cargo run -p shared --example access_logs_check
//! ```
//!
//! 只读写自己造的那几行（id 随机），结束时清理干净；不会碰历史数据。

use chrono::{DateTime, TimeDelta, Utc};
use shared::database::Database;
use shared::database::access::{
    DatabaseAccessLogsModifyRepository, DatabaseAccessLogsRepository,
};
use shared::models::access::{
    AccessCreateRequest, AccessCreateResponse, AccessInsertRequestSize, AccessInsertResponseSize,
    AccessUpdateRequestSize, AccessUpdateResponseSize, AccessVersion,
};
use simple_shared::objectid::ObjectId;

fn main() -> anyhow::Result<()> {
    let url = std::env::var("DATABASE_URL").map_err(|_| anyhow::anyhow!("DATABASE_URL not set"))?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(run(&url))
}

async fn run(url: &str) -> anyhow::Result<()> {
    let db = Database::new(url, 2).await?;

    // 1) 主表写入幂等：同一条日志写两次必须只有一行
    //    （刷盘"部分成功后整批重试"就依赖这一点）。
    let id = ObjectId::new();
    let now = db.get_real_database_time().await?;
    let request = AccessCreateRequest {
        id,
        host: "check.local".into(),
        method: "GET".into(),
        path: "/check".into(),
        headers: vec![("x-check".into(), "1".into())],
        http_version: AccessVersion::HTTP11,
        remote_addr: "127.0.0.1".into(),
        body_length: 11,
        requested_at: now,
        website_id: None,
    };
    db.insert_batch_access_requests(vec![request.clone()]).await?;
    db.insert_batch_access_requests(vec![request]).await?;
    let req_rows: i64 = count(&db, "access_request_logs", id).await?;
    assert_eq!(req_rows, 1, "请求日志必须幂等（期望 1 行，实际 {req_rows}）");

    let response = AccessCreateResponse {
        id,
        status: 200,
        headers: vec![],
        body_length: 22,
        http_version: AccessVersion::HTTP11,
        responsed_at: now,
        backend_responsed_at: Some(now),
        website_id: None,
    };
    db.insert_batch_access_responses(vec![response.clone()]).await?;
    db.insert_batch_access_responses(vec![response]).await?;
    let resp_rows: i64 = count(&db, "access_response_logs", id).await?;
    assert_eq!(resp_rows, 1, "响应日志必须幂等（期望 1 行，实际 {resp_rows}）");

    // 2) size 明细按 (id, 秒) 累加：同一秒两块 + 下一秒一块 → 2 行。
    let base: DateTime<Utc> = now;
    db.insert_batch_access_response_increase_size_logs(vec![
        AccessInsertResponseSize::new(id, 10, base),
        AccessInsertResponseSize::new(id, 25, base + TimeDelta::milliseconds(400)),
        AccessInsertResponseSize::new(id, 7, base + TimeDelta::seconds(1)),
    ])
    .await?;
    let size_rows: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT extract(epoch FROM at_second)::bigint, body_length::bigint \
           FROM access_response_size_logs WHERE response_id = $1 ORDER BY at_second",
    )
    .bind(id)
    .fetch_all(&db.pool)
    .await?;
    assert_eq!(
        size_rows,
        vec![(base.timestamp(), 35), (base.timestamp() + 1, 7)],
        "同一秒必须累加成一行、跨秒必须分开"
    );

    // 3) 父行守卫：父行不存在时静默跳过，绝不抛外键错误（否则整批刷盘会被毒化）。
    let orphan = ObjectId::new();
    db.insert_batch_access_response_increase_size_logs(vec![AccessInsertResponseSize::new(
        orphan, 5, base,
    )])
    .await?;
    let orphan_rows: i64 = count(&db, "access_response_size_logs", orphan).await?;
    assert_eq!(orphan_rows, 0, "父行缺失的 size 明细必须被静默跳过");

    // 4) 请求方向同理。
    db.insert_batch_access_request_increase_size_logs(vec![
        AccessInsertRequestSize::new(id, 3, base),
        AccessInsertRequestSize::new(id, 4, base),
    ])
    .await?;
    let req_size: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(body_length), 0)::bigint FROM access_request_size_logs \
          WHERE request_id = $1",
    )
    .bind(id)
    .fetch_one(&db.pool)
    .await?;
    assert_eq!(req_size, 7, "请求 size 明细必须累加");

    // 5) 主表 body_length 的批量 UPDATE 走的是 `id` 谓词（分区表上仍应命中）。
    db.update_batch_access_request_size_logs(vec![AccessUpdateRequestSize {
        id,
        body_length: 99,
    }])
    .await?;
    db.update_batch_access_response_size_logs(vec![AccessUpdateResponseSize {
        id,
        body_length: 88,
    }])
    .await?;

    // 6) 面板读路径：QPS / 汇总 / 按站点今日指标 / 地图 IP 聚合。
    let qps = db.get_qps_per_second(60).await?;
    let access = db.get_access_info(1).await?;
    let today = db.get_today_metrics_info_of_websites().await?;
    let ips = db.get_requests_of_ips(1).await?;
    println!(
        "读取检查: qps 序列 {} 点, 总请求 {}, 总 IP {}, 今日站点 {} 个, IP 聚合 {} 项",
        qps.data.len(),
        access.total_requests,
        access.total_ips,
        today.len(),
        ips.len()
    );

    // 7) 清理本次造的数据。
    cleanup(&db, id, orphan, base).await?;
    println!("✅ 访问日志读写检查通过（表结构：v2 分区表或 v1 均适用）");
    Ok(())
}

async fn count(db: &Database, table: &str, id: ObjectId) -> anyhow::Result<i64> {
    let id_col = if table.contains("response") {
        "response_id"
    } else {
        "request_id"
    };
    let sql = if table.ends_with("_size_logs") {
        format!("SELECT count(*) FROM {table} WHERE {id_col} = $1")
    } else {
        format!("SELECT count(*) FROM {table} WHERE id = $1")
    };
    Ok(sqlx::query_scalar(&sql).bind(id).fetch_one(&db.pool).await?)
}

async fn cleanup(
    db: &Database,
    id: ObjectId,
    orphan: ObjectId,
    base: DateTime<Utc>,
) -> anyhow::Result<()> {
    // 只删自己造的行：用完整主键条件，避免误删。
    for (table, id_col) in [
        ("access_response_size_logs", "response_id"),
        ("access_request_size_logs", "request_id"),
    ] {
        sqlx::query(&format!(
            "DELETE FROM {table} WHERE ({id_col} = $1 OR {id_col} = $2) \
               AND at_second IS NOT NULL \
               AND at_second >= $3::timestamptz - INTERVAL '5 seconds' \
               AND at_second <= $3::timestamptz + INTERVAL '5 seconds'"
        ))
        .bind(id)
        .bind(orphan)
        .bind(base)
        .execute(&db.pool)
        .await?;
    }
    sqlx::query("DELETE FROM access_response_logs WHERE id = $1")
        .bind(id)
        .execute(&db.pool)
        .await?;
    sqlx::query("DELETE FROM access_request_logs WHERE id = $1")
        .bind(id)
        .execute(&db.pool)
        .await?;
    Ok(())
}
