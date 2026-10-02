use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};
use tracing::{Level, event};

use crate::{
    database::Database,
    models::certificate::{
        CreateCertificate, CreateCertificateMethod, DatabaseCertificate, NeedSignCertificate,
        UpdateCertificate,
    },
    objectid::ObjectId,
};

/// 超过这个时长仍未释放的签发认领视为**过期**，可被重新认领。
///
/// 存在的意义：签发进程被强杀（SIGKILL / OOM / `docker kill`）时
/// `SigningGuard::drop` 不会执行，`signing_started_at` 会永远留在旧值上，
/// 该证书再也不会被选中续签。10 分钟远大于正常签发耗时（DNS-01 挑战通常
/// 数十秒），因此不会误抢正在进行的签发。
const STALE_SIGNING_CLAIM: &str = "10 minutes";

#[async_trait]
pub trait DatabaseCertificateInitializer {
    async fn initialize_certificates(&self, tx: &mut Transaction<'_, Postgres>) -> Result<()>;
}

#[async_trait]
impl DatabaseCertificateInitializer for Database {
    async fn initialize_certificates(&self, tx: &mut Transaction<'_, Postgres>) -> Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS certificates (
                id TEXT PRIMARY KEY,
                name TEXT,
                hostnames TEXT[],
                fullchain TEXT,
                private_key TEXT,
                dns_provider_id TEXT,
                email TEXT,
                expires_at TIMESTAMPTZ,
                created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                FOREIGN KEY (dns_provider_id) REFERENCES dns_providers(id) ON DELETE SET NULL
            )
        "#,
        )
        .execute(&mut **tx)
        .await?;

        // `get_will_sign_certificates` 每 30 秒执行一次且谓词落在 expires_at 上，
        // 没有索引就是全表扫描（ISSUES.md P0-13）。
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_certificates_expires_at ON certificates (expires_at)",
        )
        .execute(&mut **tx)
        .await?;

        // 签发互斥标记（ISSUES.md P0-14）。
        //
        // 原实现只用进程内 `PENDINGS: HashMap` 去重，多副本或滚动更新期间两个实例
        // 会同时为同一张证书发起 ACME 签发，既重复消耗 ZeroSSL 配额，又会互相覆盖
        // fullchain / private_key。该列用于抢占式认领，失败重试时也会被释放。
        sqlx::query(
            "ALTER TABLE certificates ADD COLUMN IF NOT EXISTS signing_started_at TIMESTAMPTZ",
        )
        .execute(&mut **tx)
        .await?;

        self.create_trigger_notify_tx(tx, "certificates").await?;

        Ok(())
    }
}

#[async_trait]
pub trait DatabaseCertificateRepository {
    async fn get_certificates(&self) -> Result<Vec<DatabaseCertificate>>;
    async fn get_certificates_before_updated_at(
        &self,
        before: &chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<DatabaseCertificate>>;
    async fn get_will_sign_certificates(&self) -> Result<Vec<NeedSignCertificate>>;
    async fn get_total_of_certificates(&self) -> Result<usize>;
    async fn get_certificates_by_page(
        &self,
        page: usize,
        limit: usize,
    ) -> Result<Vec<DatabaseCertificate>>;
}

#[async_trait]
pub trait DatabaseCertificateModifiyRepository {
    async fn update_certificate(&self, cert: &UpdateCertificate) -> Result<()>;
    async fn create_certificate(&self, cert: &CreateCertificate) -> Result<DatabaseCertificate>;
    /// 以**数据库行锁**抢占式认领一张待签发的证书。
    ///
    /// 返回 `Some` 表示本实例成功认领，调用方最后必须调用
    /// [`DatabaseCertificateModifiyRepository::release_certificate_signing`] 释放。
    /// 返回 `None` 表示当前没有可认领的证书（都已被其它实例锁住）。
    ///
    /// 关键（ISSUES.md P0-14）：`FOR UPDATE SKIP LOCKED` 让并发实例直接跳过已被
    /// 锁住的行，而不是排队等待后再重复签发，从而避免多副本重复消耗 ACME 配额，
    /// 以及互相覆盖 fullchain / private_key。
    async fn try_claim_certificate_signing(&self) -> Result<Option<NeedSignCertificate>>;
    /// 释放签发认领。成功签发或失败需要重试时都必须调用。
    async fn release_certificate_signing(&self, id: &ObjectId) -> Result<()>;
}

#[async_trait]
impl DatabaseCertificateRepository for Database {
    async fn get_certificates(&self) -> Result<Vec<DatabaseCertificate>> {
        let certs = sqlx::query_as::<_, DatabaseCertificate>(
            "SELECT id, name, hostnames, fullchain, private_key, dns_provider_id, email, expires_at, created_at, updated_at FROM certificates",
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(certs)
    }

    async fn get_certificates_before_updated_at(
        &self,
        before: &DateTime<Utc>,
    ) -> Result<Vec<DatabaseCertificate>> {
        let certs = sqlx::query_as::<_, DatabaseCertificate>
            ("SELECT id, name, hostnames, fullchain, private_key, dns_provider_id, email, expires_at, created_at, updated_at FROM certificates WHERE updated_at > $1")
            .bind(before)
            .fetch_all(&self.pool)
            .await?;
        Ok(certs)
    }

    async fn get_will_sign_certificates(&self) -> Result<Vec<NeedSignCertificate>> {
        // 修正（ISSUES.md P0-13）：原条件是 `expires_at < NOW() - '7 days'`，
        // 即「已过期超过 7 天」才开始续签，证书会在到期后（且再等 7 天）才被续签，
        // 期间站点 HTTPS 完全中断。正确语义是「将在 7 天内到期或已过期」。
        let certs = sqlx::query_as::<_, NeedSignCertificate>(
            "SELECT id, name, hostnames, dns_provider_id FROM certificates \
             WHERE (expires_at IS NULL OR expires_at < NOW() + '7 days'::INTERVAL) \
               AND dns_provider_id IS NOT NULL AND email IS NOT NULL",
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(certs)
    }

    async fn get_total_of_certificates(&self) -> Result<usize> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(id) FROM certificates")
            .fetch_one(&self.pool)
            .await?;

        Ok(count as usize)
    }

    async fn get_certificates_by_page(
        &self,
        page: usize,
        limit: usize,
    ) -> Result<Vec<DatabaseCertificate>> {
        let certificates: Vec<DatabaseCertificate> = sqlx::query_as(
            r#"SELECT * FROM certificates ORDER BY created_at DESC LIMIT $1 OFFSET $2"#,
        )
        .bind(limit as i64)
        .bind((std::cmp::max(0, page) * limit) as i64)
        .fetch_all(&self.pool)
        .await?;
        Ok(certificates)
    }
}

#[async_trait]
impl DatabaseCertificateModifiyRepository for Database {
    async fn update_certificate(&self, cert: &UpdateCertificate) -> Result<()> {
        sqlx::query(
            "UPDATE certificates SET fullchain = $1, private_key = $2, expires_at = $3, updated_at = NOW() WHERE id = $4",
        )
        .bind(&cert.fullchain)
        .bind(&cert.private_key)
        .bind(cert.expires_at()?)
        .bind(cert.id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn create_certificate(&self, cert: &CreateCertificate) -> Result<DatabaseCertificate> {
        let res = match &cert.content {
            CreateCertificateMethod::AUTO(context) => {
                sqlx::query_as::<_, DatabaseCertificate>(
                    r#"
                    INSERT INTO certificates (id, name, hostnames, dns_provider_id, email) VALUES ($1, $2, $3, $4, $5)
                    RETURNING *
                "#,
                )
                .bind(ObjectId::new())
                .bind(&cert.name)
                .bind(&context.hostnames)
                .bind(context.dns_provider_id)
                .bind(&context.email)
                .fetch_one(&self.pool)
                .await?
            },
            CreateCertificateMethod::MANUAL(context) => {
                // println!("test");
                sqlx::query_as::<_, DatabaseCertificate>(
                    r#"
                    INSERT INTO certificates (id, name, hostnames, fullchain, private_key, expires_at) VALUES ($1, $2, $3, $4, $5, $6)
                    RETURNING *
                "#,
                )
                .bind(ObjectId::new())
                .bind(&cert.name)
                .bind(&context.hostnames()?)
                .bind(&context.fullchain)
                .bind(&context.private_key)
                .bind(context.expires_at()?)
                .fetch_one(&self.pool)
                .await?
            }
        };
        Ok(res)
    }

    async fn try_claim_certificate_signing(&self) -> Result<Option<NeedSignCertificate>> {
        // 在显式事务里「选中并锁行 → 打标记 → 提交」。`FOR UPDATE SKIP LOCKED`
        // 让并发实例直接跳过已被锁住的行，而不是排队等待后再重复签发（P0-14）。
        // 用 `clock_timestamp()` 而不是 `NOW()`：后者是事务开始时间，
        // 长事务会让标记时间失真（与 P0-9 同一类问题）。
        //
        // 关于 `signing_started_at < clock_timestamp() - STALE_SIGNING_CLAIM`：
        // 释放认领原本只由 `SigningGuard::drop` 负责，而进程被 SIGKILL / OOM /
        // `docker kill` 打断时 `Drop` 根本不会执行 → 该行永远停留在"已认领"状态，
        // 再也不被任何实例选中，证书静默到期、HTTPS 中断（P0-13 同类后果）。
        // 这里把"过期的认领"视同未认领，使崩溃后能自动恢复，无需人工改库。
        let mut tx = self.pool.begin().await?;
        let cert = sqlx::query_as::<_, NeedSignCertificate>(
            r#"
            SELECT id, name, hostnames, dns_provider_id, email
              FROM certificates
             WHERE (signing_started_at IS NULL
                    OR signing_started_at < clock_timestamp() - $1::INTERVAL)
               AND (expires_at IS NULL OR expires_at < NOW() + '7 days'::INTERVAL)
               AND dns_provider_id IS NOT NULL
               AND email IS NOT NULL
             ORDER BY expires_at ASC NULLS FIRST
             LIMIT 1
             FOR UPDATE SKIP LOCKED
            "#,
        )
        .bind(STALE_SIGNING_CLAIM)
        .fetch_optional(&mut *tx)
        .await?;

        if let Some(cert) = &cert {
            sqlx::query(
                "UPDATE certificates SET signing_started_at = clock_timestamp() WHERE id = $1",
            )
            .bind(cert.id)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;

        if let Some(cert) = &cert {
            // 被回收的过期认领要留下痕迹：如果频繁出现，说明签发进程在反复崩溃。
            event!(
                Level::WARN,
                "Claimed certificate {} for signing (a stale claim older than {} was recycled, \
                 which usually means a previous signing process died without releasing it)",
                cert.id,
                STALE_SIGNING_CLAIM
            );
        }
        Ok(cert)
    }

    async fn release_certificate_signing(&self, id: &ObjectId) -> Result<()> {
        sqlx::query("UPDATE certificates SET signing_started_at = NULL WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
