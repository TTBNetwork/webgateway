-- WebGateway v2 schema version 5: JWT revocation counter.
--
-- Every user has a monotonic token_version. JWTs embed the version they were
-- issued with; authenticate() rejects tokens whose version no longer matches
-- the row. Bumping the counter (logout, admin revoke, role/TOTP change)
-- invalidates all existing tokens for that user without deleting the account.

ALTER TABLE users
    ADD COLUMN token_version INTEGER NOT NULL DEFAULT 0;

-- Mark this schema as ready. Must be the last statement of the migration.
SELECT wg_mark_schema_version(5);
