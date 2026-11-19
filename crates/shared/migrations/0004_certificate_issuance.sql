-- WebGateway v2 schema version 4: certificate issuance metadata.
--
-- Certificates are either uploaded manually (fullchain/private_key present) or
-- created as a pending ACME order (fullchain NULL, auto_renew = true). The
-- dashboard scheduler claims pending/expiring rows through signing_started_at.
--
-- dns_zone is the parent zone used for DNS-01 TXT records (for example
-- "atxa.top" for "cert-test.atxa.top"); last_error keeps the most recent
-- issuance failure visible in the panel and in logs.

ALTER TABLE certificates
    ADD COLUMN auto_renew BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN dns_zone TEXT,
    ADD COLUMN last_error TEXT;

CREATE INDEX certificates_renewal_idx
    ON certificates (expires_at NULLS FIRST)
    WHERE auto_renew;

-- Mark this schema as ready. Must be the last statement of the migration.
SELECT wg_mark_schema_version(4);
