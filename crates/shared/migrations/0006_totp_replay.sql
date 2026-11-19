-- WebGateway v2 schema version 6: TOTP replay protection.
--
-- RFC 6238 requires a TOTP code to be accepted at most once. users.last_totp_step
-- stores the highest step already consumed by login; a code whose step is not
-- strictly greater is rejected. Combined with token_version, this prevents a
-- captured code from being replayed inside its 30-second (plus skew) window.

ALTER TABLE users
    ADD COLUMN last_totp_step BIGINT;

-- Mark this schema as ready. Must be the last statement of the migration.
SELECT wg_mark_schema_version(6);
