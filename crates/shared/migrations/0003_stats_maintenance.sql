-- WebGateway v2 schema version 3: statistics maintenance state and partitions.
--
-- Two production problems motivated this migration:
--   * weekly access-log partitions were only created by migration 0001, so a
--     long-running deployment eventually lost log inserts when the current week
--     crossed the last pre-created partition;
--   * the stats rollup only looked at the most recent 90s, so any master outage
--     longer than that permanently lost per-second/minute aggregation.
--
-- stats_state stores the watermark consumed by the master stats task. The task
-- backfills the gap from raw logs (kept for 7 days) and then advances it.
--
-- Partitions are dropped whole once every row inside them is older than the raw
-- log retention window; this avoids row-by-row DELETE scans on dead partitions.

CREATE TABLE stats_state (
    id                 SMALLINT PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    last_aggregated_at TIMESTAMPTZ,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);

INSERT INTO stats_state (id) VALUES (1) ON CONFLICT (id) DO NOTHING;

CREATE TRIGGER stats_state_updated_at
    BEFORE UPDATE ON stats_state
    FOR EACH ROW EXECUTE FUNCTION wg_update_updated_at();

-- Drop weekly partitions whose entire range is older than p_retention_days.
-- The 7-day floor keeps an operator from accidentally dropping the active week.
CREATE OR REPLACE FUNCTION wg_drop_expired_weekly_partitions(p_retention_days int DEFAULT 7)
RETURNS int LANGUAGE plpgsql AS $fn$
DECLARE
    v_cutoff  date := (now() - make_interval(days => p_retention_days))::date;
    v_rec     record;
    v_dropped int := 0;
    v_start   date;
BEGIN
    IF p_retention_days < 7 THEN
        RAISE EXCEPTION 'retention days must be >= 7, got %', p_retention_days;
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

-- Make sure the new function is exercised immediately: current week + 3 ahead.
SELECT wg_ensure_upcoming_weeks(3);

-- Mark this schema as ready. Must be the last statement of the migration.
SELECT wg_mark_schema_version(3);
