CREATE TABLE IF NOT EXISTS access_request_logs (
    id                      TEXT PRIMARY KEY NOT NULL,
    host                    TEXT NOT NULL,
    method                  TEXT NOT NULL,
    path                    TEXT NOT NULL,
    headers                 JSONB NOT NULL DEFAULT '[]',
    http_version            TEXT NOT NULL,
    remote_addr             TEXT NOT NULL,
    body_length             uint8 NOT NULL,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    requested_at            TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    website_id              TEXT
);

CREATE TABLE IF NOT EXISTS access_response_logs (
    id                      TEXT PRIMARY KEY NOT NULL REFERENCES access_request_logs(id),
    status                  UINT2 NOT NULL,
    headers                 JSONB NOT NULL DEFAULT '[]',
    body_length             uint8,
    http_version            TEXT NOT NULL,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    responsed_at            TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    backend_responsed_at    TIMESTAMPTZ DEFAULT NOW(),
    website_id              TEXT
);

-- 请求/响应体大小明细：**每个(请求, 秒)一行**，用于统计"每秒传了多少字节"。
--
-- 为什么需要 UNIQUE (xxx_id, at_second)：
--   1. 粒度：保留每秒颗粒度 —— 这是这两张表的用途（按秒统计流量大小）。
--   2. 去重：`StatisticsIncoming::poll_frame` 每读到一个 body chunk 就上报一次
--      （一个响应体常被切成 7+ 块），若每块插一行，生产库实测该表膨胀到
--      **1600 万行 / 3.6 GB（请求主表的 7.03 倍）**，且近 24 小时仍以 6.4 倍重复率增长
--      （单条 response_id 最多挂 262791 行）。
--      有了唯一约束后，同一秒内的多块用 `ON CONFLICT ... DO UPDATE SET body_length =
--      body_length + EXCLUDED.body_length` **累加**成一行：粒度不丢，行数按秒收敛。
--
-- `id` 列保留（主键），便于与既有数据/工具兼容，但唯一性由 (xxx_id, at_second) 表达。
CREATE TABLE IF NOT EXISTS access_request_size_logs (
    id                      TEXT PRIMARY KEY NOT NULL,
    request_id              TEXT NOT NULL REFERENCES access_request_logs(id),
    body_length             uint8 NOT NULL,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS access_response_size_logs (
    id                      TEXT PRIMARY KEY NOT NULL,
    response_id             TEXT NOT NULL REFERENCES access_response_logs(id),
    body_length             uint8 NOT NULL,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_requested_at ON access_request_logs (requested_at);
CREATE INDEX IF NOT EXISTS idx_responsed_at ON access_response_logs (responsed_at);
CREATE INDEX IF NOT EXISTS idx_access_response_logs_status ON access_response_logs (status);
CREATE INDEX IF NOT EXISTS idx_access_request_logs_website_id ON access_request_logs (website_id);
CREATE INDEX IF NOT EXISTS idx_access_response_logs_website_id ON access_response_logs (website_id);
CREATE INDEX IF NOT EXISTS idx_access_request_size_logs_created_at ON access_request_size_logs (created_at);
CREATE INDEX IF NOT EXISTS idx_access_response_size_logs_created_at ON access_response_size_logs (created_at);
CREATE INDEX IF NOT EXISTS idx_access_request_size_logs_id ON access_request_size_logs (id);
CREATE INDEX IF NOT EXISTS idx_access_response_size_logs_id ON access_response_size_logs (id);
CREATE INDEX IF NOT EXISTS idx_access_request_size_logs_req_id ON access_request_size_logs (request_id);
CREATE INDEX IF NOT EXISTS idx_access_response_size_logs_resp_id ON access_response_size_logs (response_id);
-- 访问地图按 remote_addr 聚合（ISSUES.md P1-1）：与 requested_at 组成复合索引，兼顾窗口过滤。
CREATE INDEX IF NOT EXISTS idx_access_request_logs_remote_addr_time ON access_request_logs (remote_addr, requested_at);

-- 注意：视图内的 ORDER BY 是冗余的（外层查询总会再排序），
-- 且会让优化器难以对视图做谓词下推。QPS 查询已改为直接过滤 requested_at（见
-- crates/shared/src/database/access.rs），这两个视图仅为兼容保留（ISSUES.md P0-3 / P2-1）。
CREATE OR REPLACE VIEW qps_per_second AS
    SELECT
        date_trunc('second', requested_at) AS time,
        COUNT(req.id) AS total_requests,
        COUNT(req.id) AS qps  
    FROM access_request_logs req
    GROUP BY time;

CREATE OR REPLACE VIEW qps_per_5s AS
    SELECT
        to_timestamp(floor(extract(epoch from requested_at) / 5) * 5) AS time,
        COUNT(req.id) AS total_requests,                
        COUNT(req.id) / 5.0 AS avg_qps                   
    FROM access_request_logs req
    GROUP BY time;

CREATE OR REPLACE VIEW daily_traffic_by_website AS
    SELECT
        DATE(req.requested_at) AS day,
        COALESCE(req.website_id, 'global') AS website_id,
        COUNT(req.id) AS total_requests,
        COUNT(resp.id) AS total_responses,
        COALESCE(SUM(req.body_length), 0) AS total_request_bytes,
        COALESCE(SUM(resp.body_length), 0) AS total_response_bytes,
        COALESCE(SUM(req.body_length), 0) + COALESCE(SUM(resp.body_length), 0) AS total_bytes
    FROM access_request_logs req
    LEFT JOIN access_response_logs resp ON req.id = resp.id
    GROUP BY day, req.website_id;
