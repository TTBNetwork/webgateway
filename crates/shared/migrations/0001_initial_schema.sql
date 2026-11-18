-- WebGateway v2 initial schema (migration version 1).
--
-- This migration is executed by the dashboard (master) process only.
-- The gateway has no migration code path and must never run DDL.
--
-- Requires the internal PostgreSQL image (see postgresql-dockerfile):
--   * uint128   -> unsigned integer family uint1/uint2/uint4/uint8/uint16
--   * btree_gin -> GIN opclass for TEXT (websites.name)
--
-- v2 starts from a fresh database: there is no v1 data migration and no
-- destructive schema-version gating.

CREATE EXTENSION IF NOT EXISTS uint128;
CREATE EXTENSION IF NOT EXISTS btree_gin;

-- ---------------------------------------------------------------------------
-- Readiness marker (single row). The gateway polls this table before starting.
-- Each migration bumps it as its very last statement, inside the migration
-- transaction, so "marker visible" implies "schema is fully committed".
-- ---------------------------------------------------------------------------
CREATE TABLE schema_state (
    id         SMALLINT PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    version    INTEGER NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE OR REPLACE FUNCTION wg_update_updated_at()
RETURNS trigger AS $fn$
BEGIN
    -- clock_timestamp() instead of now(): now() is the transaction start time,
    -- which can be older than a gateway's sync watermark after a long tx.
    NEW.updated_at := clock_timestamp();
    RETURN NEW;
END;
$fn$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION wg_notify_change()
RETURNS trigger AS $fn$
DECLARE
    payload jsonb;
BEGIN
    payload := jsonb_build_object(
        'table', TG_TABLE_NAME,
        'id', to_jsonb(NEW) ->> 'id',
        'key', COALESCE(to_jsonb(NEW) ->> 'id', to_jsonb(NEW) ->> 'key'),
        'updated_at', to_jsonb(NEW) ->> 'updated_at'
    );
    PERFORM pg_notify(TG_TABLE_NAME || '_updater', payload::text);
    RETURN NEW;
END;
$fn$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION wg_mark_schema_version(p_version INTEGER)
RETURNS void AS $fn$
BEGIN
    INSERT INTO schema_state (id, version, created_at, updated_at)
    VALUES (1, p_version, clock_timestamp(), clock_timestamp())
    ON CONFLICT (id) DO UPDATE
       SET version = EXCLUDED.version,
           updated_at = clock_timestamp();
END;
$fn$ LANGUAGE plpgsql;

-- ---------------------------------------------------------------------------
-- Configurations (key/value). Notify channel: configurations_updater.
-- ---------------------------------------------------------------------------
CREATE TABLE configurations (
    key        TEXT PRIMARY KEY,
    value      JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TRIGGER configurations_updated_at
    BEFORE UPDATE ON configurations
    FOR EACH ROW EXECUTE FUNCTION wg_update_updated_at();

CREATE TRIGGER configurations_notify
    AFTER INSERT OR UPDATE ON configurations
    FOR EACH ROW EXECUTE FUNCTION wg_notify_change();

-- ---------------------------------------------------------------------------
-- Dashboard users (TOTP-only auth, per-user JWT secret, roles).
-- ---------------------------------------------------------------------------
CREATE TABLE users (
    id          TEXT PRIMARY KEY,
    username    TEXT NOT NULL,
    totp_secret TEXT NOT NULL,
    jwt_secret  TEXT NOT NULL,
    role        TEXT NOT NULL DEFAULT 'view' CHECK (role IN ('admin', 'user', 'view')),
    last_login  TIMESTAMPTZ,
    last_ip     TEXT,
    addresses   TEXT[] NOT NULL DEFAULT '{}',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX users_username_lower_idx ON users (LOWER(username));

CREATE TRIGGER users_updated_at
    BEFORE UPDATE ON users
    FOR EACH ROW EXECUTE FUNCTION wg_update_updated_at();

CREATE TABLE users_client_secrets (
    user_id    TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    secret     TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (user_id, secret)
);

CREATE INDEX users_client_secrets_user_id_idx ON users_client_secrets (user_id);
CREATE INDEX users_client_secrets_secret_idx ON users_client_secrets (secret);

CREATE VIEW users_info AS
SELECT
    id, username, totp_secret, jwt_secret, role,
    created_at, updated_at, last_login, last_ip, addresses,
    (SELECT COUNT(*) FROM users_client_secrets cs WHERE cs.user_id = users.id) AS client_secrets_count,
    EXISTS (SELECT 1 FROM users_client_secrets cs WHERE cs.user_id = users.id) AS bound_totp
FROM users;

CREATE TABLE web_log (
    id         TEXT PRIMARY KEY,
    user_id    TEXT NOT NULL REFERENCES users (id),
    content    JSONB NOT NULL DEFAULT '{}'::jsonb,
    address    TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX web_log_user_created_idx ON web_log (user_id, created_at DESC);
CREATE INDEX web_log_created_idx ON web_log (created_at DESC);

-- ---------------------------------------------------------------------------
-- DNS providers. Notify channel: dns_providers_updater.
-- ---------------------------------------------------------------------------
CREATE TABLE dns_providers (
    id         TEXT PRIMARY KEY,
    name       TEXT,
    provider   JSONB NOT NULL,
    domains    TEXT[] NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX dns_providers_created_at_idx ON dns_providers (created_at DESC);

CREATE TRIGGER dns_providers_updated_at
    BEFORE UPDATE ON dns_providers
    FOR EACH ROW EXECUTE FUNCTION wg_update_updated_at();

CREATE TRIGGER dns_providers_notify
    AFTER INSERT OR UPDATE ON dns_providers
    FOR EACH ROW EXECUTE FUNCTION wg_notify_change();

-- ---------------------------------------------------------------------------
-- Certificates. Notify channel: certificates_updater.
-- ---------------------------------------------------------------------------
CREATE TABLE certificates (
    id                 TEXT PRIMARY KEY,
    name               TEXT,
    hostnames          TEXT[] NOT NULL DEFAULT '{}',
    fullchain          TEXT,
    private_key        TEXT,
    dns_provider_id    TEXT REFERENCES dns_providers (id) ON DELETE SET NULL,
    email              TEXT,
    expires_at         TIMESTAMPTZ,
    signing_started_at TIMESTAMPTZ,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX certificates_expires_at_idx ON certificates (expires_at);
CREATE INDEX certificates_dns_provider_id_idx ON certificates (dns_provider_id);
CREATE INDEX certificates_created_at_idx ON certificates (created_at DESC);

CREATE TRIGGER certificates_updated_at
    BEFORE UPDATE ON certificates
    FOR EACH ROW EXECUTE FUNCTION wg_update_updated_at();

CREATE TRIGGER certificates_notify
    AFTER INSERT OR UPDATE ON certificates
    FOR EACH ROW EXECUTE FUNCTION wg_notify_change();

-- ---------------------------------------------------------------------------
-- Websites / routing. Notify channel: websites_updater.
-- ports uses the uint2 type family (0..65535) from the uint128 extension.
-- ---------------------------------------------------------------------------
CREATE TABLE websites (
    id           TEXT PRIMARY KEY,
    name         TEXT,
    hosts        TEXT[] NOT NULL DEFAULT '{}',
    ports        uint2[] NOT NULL DEFAULT '{}',
    certificates TEXT[] NOT NULL DEFAULT '{}',
    backends     JSONB NOT NULL DEFAULT '[]'::jsonb,
    config       JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX websites_hosts_gin ON websites USING GIN (hosts);
CREATE INDEX websites_name_gin ON websites USING GIN (name);
CREATE INDEX websites_created_at_idx ON websites (created_at DESC);

CREATE TRIGGER websites_updated_at
    BEFORE UPDATE ON websites
    FOR EACH ROW EXECUTE FUNCTION wg_update_updated_at();

CREATE TRIGGER websites_notify
    AFTER INSERT OR UPDATE ON websites
    FOR EACH ROW EXECUTE FUNCTION wg_notify_change();

-- ---------------------------------------------------------------------------
-- Gateway registry and online/offline marking (medium priority feature).
--
-- The gateway upserts this row on startup (status = 'online', started_at), keeps
-- last_seen_at fresh with a heartbeat, and marks status = 'offline' + stopped_at
-- on graceful shutdown. A crashed gateway stays 'online' in this table, so the
-- authoritative "online" check must also consider heartbeat freshness; see the
-- gateways_online view below.
-- ---------------------------------------------------------------------------
CREATE TABLE gateways (
    id           TEXT PRIMARY KEY,
    machine_id   TEXT NOT NULL UNIQUE,
    name         TEXT,
    country      TEXT,
    city         TEXT,
    address      TEXT,
    version      TEXT,
    status       TEXT NOT NULL DEFAULT 'offline' CHECK (status IN ('online', 'offline')),
    last_seen_at TIMESTAMPTZ,
    started_at   TIMESTAMPTZ,
    stopped_at   TIMESTAMPTZ,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX gateways_status_last_seen_idx ON gateways (status, last_seen_at DESC);

CREATE TRIGGER gateways_updated_at
    BEFORE UPDATE ON gateways
    FOR EACH ROW EXECUTE FUNCTION wg_update_updated_at();

-- A gateway counts as online only while its heartbeat is fresh. The gateway
-- heartbeat interval is expected to be well below this 30s threshold.
CREATE VIEW gateways_online AS
SELECT id, machine_id, name, country, city, address, version, last_seen_at
FROM gateways
WHERE status = 'online'
  AND last_seen_at >= now() - INTERVAL '30 seconds';

-- ---------------------------------------------------------------------------
-- Access logs, weekly RANGE partitions. Partition key is part of every PK.
-- There are no foreign keys on partitioned log tables (no parent-row reference
-- is possible); referential integrity is enforced by the writer.
-- All four tables are owned by the log-writer task that follows this step.
-- ---------------------------------------------------------------------------
CREATE TABLE access_request_logs (
    id           TEXT NOT NULL,
    host         TEXT NOT NULL,
    method       TEXT NOT NULL,
    path         TEXT NOT NULL,
    headers      JSONB NOT NULL DEFAULT '[]'::jsonb,
    http_version TEXT NOT NULL,
    remote_addr  TEXT NOT NULL,
    body_length  uint8 NOT NULL DEFAULT 0,
    website_id   TEXT,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    requested_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (id, requested_at)
) PARTITION BY RANGE (requested_at);

CREATE INDEX access_request_logs_requested_at_idx ON access_request_logs (requested_at);
CREATE INDEX access_request_logs_website_time_idx ON access_request_logs (website_id, requested_at);
CREATE INDEX access_request_logs_remote_time_idx ON access_request_logs (remote_addr, requested_at);
CREATE INDEX access_request_logs_id_idx ON access_request_logs (id);

-- status intentionally has no RFC range CHECK: upstreams are free to return
-- non-standard codes and we still want to record them.
CREATE TABLE access_response_logs (
    id                   TEXT NOT NULL,
    status               uint2 NOT NULL,
    headers              JSONB NOT NULL DEFAULT '[]'::jsonb,
    body_length          uint8,
    http_version         TEXT NOT NULL,
    website_id           TEXT,
    created_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    responsed_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    backend_responsed_at TIMESTAMPTZ,
    PRIMARY KEY (id, responsed_at)
) PARTITION BY RANGE (responsed_at);

CREATE INDEX access_response_logs_responsed_at_idx ON access_response_logs (responsed_at);
CREATE INDEX access_response_logs_status_time_idx ON access_response_logs (status, responsed_at);
CREATE INDEX access_response_logs_website_time_idx ON access_response_logs (website_id, responsed_at);
CREATE INDEX access_response_logs_id_idx ON access_response_logs (id);

CREATE TABLE access_request_size_logs (
    id          TEXT NOT NULL,
    request_id  TEXT NOT NULL,
    body_length uint8 NOT NULL DEFAULT 0,
    at_second   TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (id, at_second)
) PARTITION BY RANGE (at_second);

CREATE INDEX access_request_size_logs_at_second_idx ON access_request_size_logs (at_second);
CREATE UNIQUE INDEX access_request_size_logs_request_second_idx ON access_request_size_logs (request_id, at_second);

CREATE TABLE access_response_size_logs (
    id          TEXT NOT NULL,
    response_id TEXT NOT NULL,
    body_length uint8 NOT NULL DEFAULT 0,
    at_second   TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (id, at_second)
) PARTITION BY RANGE (at_second);

CREATE INDEX access_response_size_logs_at_second_idx ON access_response_size_logs (at_second);
CREATE UNIQUE INDEX access_response_size_logs_response_second_idx ON access_response_size_logs (response_id, at_second);

-- ---------------------------------------------------------------------------
-- Weekly partition management.
-- ---------------------------------------------------------------------------
CREATE OR REPLACE FUNCTION wg_access_partition_parents()
RETURNS text[] LANGUAGE sql STABLE AS $fn$
    SELECT COALESCE(array_agg(c.relname ORDER BY c.relname), ARRAY[]::text[])
      FROM pg_class c
      JOIN pg_namespace n ON n.oid = c.relnamespace
     WHERE n.nspname = 'public'
       AND c.relkind = 'p'
       AND c.relname IN (
           'access_request_logs',
           'access_response_logs',
           'access_request_size_logs',
           'access_response_size_logs'
       );
$fn$;

CREATE OR REPLACE FUNCTION wg_ensure_weekly_partition(p_parent text, p_week_start date)
RETURNS void LANGUAGE plpgsql AS $fn$
DECLARE
    v_name text;
    v_end  date := p_week_start + 7;
BEGIN
    v_name := format('%s_%s_w%s', p_parent,
                     to_char(p_week_start, 'IYYY'), to_char(p_week_start, 'IW'));
    PERFORM pg_advisory_xact_lock(hashtext(v_name));
    EXECUTE format(
        'CREATE TABLE IF NOT EXISTS %I PARTITION OF %I FOR VALUES FROM (%L) TO (%L)',
        v_name, p_parent, p_week_start, v_end);
END;
$fn$;

CREATE OR REPLACE FUNCTION wg_ensure_upcoming_weeks(p_ahead int DEFAULT 2)
RETURNS void LANGUAGE plpgsql AS $fn$
DECLARE
    v_this_week date := date_trunc('week', now())::date;
    v_table     text;
    i           int;
BEGIN
    FOREACH v_table IN ARRAY wg_access_partition_parents() LOOP
        FOR i IN -1..p_ahead LOOP
            PERFORM wg_ensure_weekly_partition(v_table, v_this_week + (i * 7));
        END LOOP;
    END LOOP;
END;
$fn$;

CREATE OR REPLACE FUNCTION wg_drop_partitions_older_than(p_days int)
RETURNS int LANGUAGE plpgsql AS $fn$
DECLARE
    v_cutoff  date := (now() - make_interval(days => p_days))::date;
    v_rec     record;
    v_dropped int := 0;
    v_start   date;
BEGIN
    IF p_days < 90 THEN
        RAISE EXCEPTION 'retention days must be >= 90, got %', p_days;
    END IF;
    FOR v_rec IN
        SELECT c.relname AS child
          FROM pg_inherits i
          JOIN pg_class c ON c.oid = i.inhrelid
          JOIN pg_class p ON p.oid = i.inhparent
         WHERE p.relname = ANY (wg_access_partition_parents())
    LOOP
        BEGIN
            v_start := to_date(
                substring(v_rec.child from '_([0-9]{4})_w[0-9]+$') || '-' ||
                substring(v_rec.child from '_w([0-9]+)$'), 'IYYY-IW');
        EXCEPTION WHEN others THEN
            CONTINUE;
        END;
        IF v_start IS NULL THEN
            CONTINUE;
        END IF;
        IF v_start + 7 <= v_cutoff THEN
            EXECUTE format('DROP TABLE IF EXISTS %I', v_rec.child);
            v_dropped := v_dropped + 1;
        END IF;
    END LOOP;
    RETURN v_dropped;
END;
$fn$;

SELECT wg_ensure_upcoming_weeks(2);

-- ---------------------------------------------------------------------------
-- Aggregated traffic statistics.
--
-- Granularity policy (decided 2026-11-18):
--   * second level: retained for the most recent 1 hour (access_stats_second)
--   * minute level: everything older than 1 hour is aggregated into
--     access_stats_minute, and the corresponding second rows are deleted
--   * daily level: access_stats_daily is rolled up from minute data
-- The rollup/pruning jobs belong to the logging task and are not installed by
-- this migration; only the target tables are created here.
-- ---------------------------------------------------------------------------
CREATE TABLE access_stats_second (
    second_at              TIMESTAMPTZ NOT NULL,
    website_id             TEXT NOT NULL DEFAULT '__unknown__',
    total_requests         BIGINT NOT NULL DEFAULT 0,
    total_responses        BIGINT NOT NULL DEFAULT 0,
    total_ips              BIGINT NOT NULL DEFAULT 0,
    e4xx_requests          BIGINT NOT NULL DEFAULT 0,
    e5xx_requests          BIGINT NOT NULL DEFAULT 0,
    backend_error_requests BIGINT NOT NULL DEFAULT 0,
    total_requests_size    uint8 NOT NULL DEFAULT 0,
    total_response_size    uint8 NOT NULL DEFAULT 0,
    created_at             TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at             TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (second_at, website_id)
);

CREATE INDEX access_stats_second_website_time_idx ON access_stats_second (website_id, second_at DESC);

CREATE TRIGGER access_stats_second_updated_at
    BEFORE UPDATE ON access_stats_second
    FOR EACH ROW EXECUTE FUNCTION wg_update_updated_at();

CREATE TABLE access_stats_minute (
    minute_at              TIMESTAMPTZ NOT NULL,
    website_id             TEXT NOT NULL DEFAULT '__unknown__',
    total_requests         BIGINT NOT NULL DEFAULT 0,
    total_responses        BIGINT NOT NULL DEFAULT 0,
    total_ips              BIGINT NOT NULL DEFAULT 0,
    e4xx_requests          BIGINT NOT NULL DEFAULT 0,
    e5xx_requests          BIGINT NOT NULL DEFAULT 0,
    backend_error_requests BIGINT NOT NULL DEFAULT 0,
    total_requests_size    uint8 NOT NULL DEFAULT 0,
    total_response_size    uint8 NOT NULL DEFAULT 0,
    created_at             TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at             TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (minute_at, website_id)
);

CREATE INDEX access_stats_minute_website_time_idx ON access_stats_minute (website_id, minute_at DESC);

CREATE TRIGGER access_stats_minute_updated_at
    BEFORE UPDATE ON access_stats_minute
    FOR EACH ROW EXECUTE FUNCTION wg_update_updated_at();

CREATE TABLE access_stats_daily (
    day                    DATE NOT NULL,
    website_id             TEXT NOT NULL DEFAULT '__unknown__',
    total_requests         BIGINT NOT NULL DEFAULT 0,
    total_responses        BIGINT NOT NULL DEFAULT 0,
    total_ips              BIGINT NOT NULL DEFAULT 0,
    e4xx_requests          BIGINT NOT NULL DEFAULT 0,
    e5xx_requests          BIGINT NOT NULL DEFAULT 0,
    backend_error_requests BIGINT NOT NULL DEFAULT 0,
    total_requests_size    uint8 NOT NULL DEFAULT 0,
    total_response_size    uint8 NOT NULL DEFAULT 0,
    created_at             TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at             TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (day, website_id)
);

CREATE INDEX access_stats_daily_website_day_idx ON access_stats_daily (website_id, day DESC);

CREATE TRIGGER access_stats_daily_updated_at
    BEFORE UPDATE ON access_stats_daily
    FOR EACH ROW EXECUTE FUNCTION wg_update_updated_at();

-- ---------------------------------------------------------------------------
-- Mark this schema as ready. Must be the last statement of the migration.
-- ---------------------------------------------------------------------------
SELECT wg_mark_schema_version(1);
