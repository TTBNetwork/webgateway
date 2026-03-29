use crate::{
    auth::{DEFAULT_ADMIN_USERNAME, generate_random_secret, get_totp_code},
    database::log::WebLogManager,
    models::{auth::{AuthVerifyTOTP, AuthVerifyTOTPType, DatabaseAuthentication, AuthInfo}, log::LogAddr},
};
use anyhow::{Result, anyhow};
use shared::{
    database::{Database, get_database},
    objectid::ObjectId,
};
use sqlx::FromRow;
use tracing::{self, Level, event};

/// 用户表的所有列名，用于查询时复用

#[async_trait::async_trait]
pub trait Authentication {
    async fn create_user(
        &self,
        username: impl Into<String> + Send,
        totp_secret: &str,
    ) -> Result<DatabaseAuthentication>;
    async fn init_authentication(&self) -> Result<()>;
    // async fn is_exists_user(&self, username: &str) -> Result<bool>;
    async fn get_user(&self, username: &str) -> Result<DatabaseAuthentication>;
    async fn get_first_user(&self) -> Result<DatabaseAuthentication>;
    async fn verify_totp(&self, auth: AuthVerifyTOTP) -> Result<bool>;
    async fn get_user_from_id(&self, id: &ObjectId) -> Result<DatabaseAuthentication>;
    async fn get_user_all_secrets(&self, id: &ObjectId) -> Result<Vec<String>>;
    async fn add_client_secret(&self, id: &ObjectId, secret: impl Into<String> + Send) -> Result<()>;
    async fn get_info_of_users(&self) -> Result<Vec<AuthInfo>>;
}

const INIT_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS users (
    id TEXT PRIMARY KEY,
    username TEXT NOT NULL,
    totp_secret TEXT NOT NULL,
    jwt_secret TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_login TIMESTAMPTZ,
    last_ip TEXT,
    addresses TEXT[] NOT NULL DEFAULT '{}',
    UNIQUE (username)
);

CREATE TABLE IF NOT EXISTS users_client_secrets (
    user_id TEXT NOT NULL REFERENCES users (id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    secret TEXT NOT NULL,
    UNIQUE (user_id, secret)
);


CREATE UNIQUE INDEX IF NOT EXISTS users_username_idx ON users (LOWER(username));
CREATE INDEX IF NOT EXISTS users_client_secrets_user_id_idx ON users_client_secrets (user_id);
CREATE INDEX IF NOT EXISTS users_client_secrets_secret_idx ON users_client_secrets (secret);

CREATE OR REPLACE VIEW users_info AS
SELECT
    id, username, totp_secret, jwt_secret,
    created_at, updated_at, last_login, last_ip, addresses,
    (SELECT COUNT(*) FROM users_client_secrets WHERE user_id = users.id) AS client_secrets_count,
    EXISTS (SELECT 1 FROM users_client_secrets cs WHERE cs.user_id = users.id) AS bound_totp
FROM users;
"#;

#[async_trait::async_trait]
impl Authentication for Database {
    async fn init_authentication(&self) -> Result<()> {
        // 初始化用户表
        sqlx::raw_sql(INIT_SQL).execute(&self.pool).await?;

        // 检查是否存在用户，若无则创建默认管理员
        let _ = match self.get_first_user().await {
            Ok(user) => user,
            Err(_) => {
                event!(Level::INFO, "No users found, creating default admin");
                let secret = generate_random_secret();
                self.create_user(DEFAULT_ADMIN_USERNAME.as_str(), &secret)
                    .await?
            }
        };

        Ok(())
    }

    async fn create_user(
        &self,
        username: impl Into<String> + Send,
        totp_secret: &str,
    ) -> Result<DatabaseAuthentication> {
        let username = username.into();
        let id = ObjectId::new();
        let jwt_secret = generate_random_secret();

        // 直接插入，依赖数据库唯一索引保证不重复
        let result = sqlx::query(
            r#"
            INSERT INTO users (id, username, totp_secret, jwt_secret)
            VALUES ($1, $2, $3, $4)
            "#,
        )
        .bind(id)
        .bind(&username)
        .bind(totp_secret)
        .bind(&jwt_secret)
        .execute(&self.pool)
        .await;

        match result {
            Ok(_) => self.get_user(&username).await,
            Err(sqlx::Error::Database(db_err)) if db_err.is_unique_violation() => {
                Err(anyhow!("User '{}' already exists", username))
            }
            Err(e) => Err(anyhow!("Failed to create user: {}", e)),
        }
    }

    async fn get_user(&self, username: &str) -> Result<DatabaseAuthentication> {
        let row = sqlx::query(r#"
            SELECT * FROM users_info
            WHERE LOWER(username) = LOWER($1)
            "#)
        .bind(username)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| anyhow!("User '{}' not found", username))?;

        Ok(DatabaseAuthentication::from_row(&row)?)
    }

    async fn get_first_user(&self) -> Result<DatabaseAuthentication> {
        let row = sqlx::query(r#"
            SELECT * FROM users_info
            ORDER BY created_at ASC
            LIMIT 1
            "#)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| anyhow!("No users found"))?;

        Ok(DatabaseAuthentication::from_row(&row)?)
    }

    async fn verify_totp(&self, auth: AuthVerifyTOTP) -> Result<bool> {
        let username = &auth.username;
        let totp = &auth.totp;
        let addr = LogAddr(auth.addr.to_string());
        let user = match self.get_user(username).await {
            Ok(user) => user,
            Err(e) => return {
                event!(Level::ERROR, "Failed to get user '{}': {}", username, e);
                Ok(false)
            }, // 用户不存在视为验证失败
        };

        // lianjie
        let client_secrets = self.get_user_all_secrets(&user.id).await?;
        let mut secrets = vec![user.totp_secret.clone()];
        if auth.verify_type == AuthVerifyTOTPType::Login {
            secrets.extend(client_secrets);
        }
        for secret in secrets {
            if  get_totp_code(username, secret)?.eq(totp) {
                get_database()
                    .add_web_log(
                        &user.id,
                        &crate::models::log::LogContent::Raw(
                            match auth.verify_type{
                                AuthVerifyTOTPType::Login => "auth.user.login.success",
                                AuthVerifyTOTPType::WantBind => "auth.user.want_bind.success",
                            }.to_string()
                        ),
                        &addr)                    .await?;
                return Ok(true);
            }
        }

            get_database()
            .add_web_log(
                &user.id,
                &crate::models::log::LogContent::Raw(
                    match auth.verify_type{
                                AuthVerifyTOTPType::Login => "auth.user.login.fail",
                                AuthVerifyTOTPType::WantBind => "auth.user.want_bind.fail",
                            }.to_string()
                ),
                &addr,
            )
            .await?;
        Ok(false)
    }

    async fn get_user_from_id(&self, user_id: &ObjectId) -> Result<DatabaseAuthentication> {
        let row = sqlx::query(r#"
            SELECT * FROM users_info
            WHERE id = $1
            "#)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| anyhow!("User with id '{}' not found", user_id))?;

        Ok(DatabaseAuthentication::from_row(&row)?)
    }

    async fn get_user_all_secrets(&self, user_id: &ObjectId) -> Result<Vec<String>> {
        let rows = sqlx::query_as::<_, (String,)>(
            r#"
            SELECT secret FROM users_client_secrets
            WHERE user_id = $1
            "#,
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(|row| row.0).collect())
    }

    async fn add_client_secret(&self, user_id: &ObjectId, secret: impl Into<String> + Send) -> Result<()> {
        let _ = sqlx::query(
            r#"
            INSERT INTO users_client_secrets (user_id, secret)
            VALUES ($1, $2)
            "#,
        ).bind(user_id)
        .bind(secret.into())
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn get_info_of_users(&self) -> Result<Vec<AuthInfo>> {
        let rows = sqlx::query_as::<_, AuthInfo>(
            r#"
            SELECT 
                *
            FROM users_info
            "#
        ).fetch_all(&self.pool)
        .await?;

        Ok(rows)
    }
}
