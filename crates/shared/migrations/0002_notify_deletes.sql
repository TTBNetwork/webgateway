-- Notify LISTEN channels on DELETE as well.
--
-- The INSERT/UPDATE triggers from migration 1 cover create and update. Deletes
-- previously went unnoticed by gateways, which had to poll periodically. The
-- gateway now relies on notifications plus a watermark check, so deletes must
-- emit an event too.

CREATE OR REPLACE FUNCTION wg_notify_delete()
RETURNS trigger AS $fn$
DECLARE
    payload jsonb;
BEGIN
    payload := jsonb_build_object(
        'table', TG_TABLE_NAME,
        'id', to_jsonb(OLD) ->> 'id',
        'key', COALESCE(to_jsonb(OLD) ->> 'id', to_jsonb(OLD) ->> 'key'),
        'operation', 'delete'
    );
    PERFORM pg_notify(TG_TABLE_NAME || '_updater', payload::text);
    RETURN OLD;
END;
$fn$ LANGUAGE plpgsql;

CREATE TRIGGER configurations_notify_delete
    AFTER DELETE ON configurations
    FOR EACH ROW EXECUTE FUNCTION wg_notify_delete();

CREATE TRIGGER dns_providers_notify_delete
    AFTER DELETE ON dns_providers
    FOR EACH ROW EXECUTE FUNCTION wg_notify_delete();

CREATE TRIGGER certificates_notify_delete
    AFTER DELETE ON certificates
    FOR EACH ROW EXECUTE FUNCTION wg_notify_delete();

CREATE TRIGGER websites_notify_delete
    AFTER DELETE ON websites
    FOR EACH ROW EXECUTE FUNCTION wg_notify_delete();

-- Mark this schema as ready. Must be the last statement of the migration.
SELECT wg_mark_schema_version(2);
