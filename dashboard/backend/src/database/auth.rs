use crate::{
    auth::{DEFAULT_ADMIN_USERNAME, generate_random_secret, get_totp_code},
    database::log::WebLogManager,
    models::{
        auth::{AuthInfo, AuthVerifyTOTP, AuthVerifyTOTPType, DatabaseAuthentication, Role},
        log::LogAddr,
    },
};
use anyhow::{Result, anyhow};
use shared::{
    database::{Database, get_database},
    objectid::ObjectId,
};
use sqlx::{FromRow, Postgres, Transaction};
use tracing::{self, Level, event};

/// 用户表的所有列名，用于查询时复用

#[async_trait::async_trait]
pub trait Authentication {
    async fn create_user(
        &self,
        username: impl Into<String> + Send,
        totp_secret: &str,
        role: Role,
    ) -> Result<DatabaseAuthentication>;
    /// 在给定事务中创建用户表与相关视图（由迁移入口在 advisory lock 下调用）。
    async fn init_authentication(&self, tx: &mut Transaction<'_, Postgres>) -> Result<()>;
    /// 表结构就绪后确保存在一个管理员账号（默认账号引导）。
    async fn ensure_default_admin(&self) -> Result<()>;
    // async fn is_exists_user(&self, username: &str) -> Result<bool>;
    async fn get_user(&self, username: &str) -> Result<DatabaseAuthentication>;
    async fn get_first_user(&self) -> Result<DatabaseAuthentication>;
    async fn verify_totp(&self, auth: AuthVerifyTOTP) -> Result<bool>;
    async fn get_user_from_id(&self, id: &ObjectId) -> Result<DatabaseAuthentication>;
    async fn get_user_all_secrets(&self, id: &ObjectId) -> Result<Vec<String>>;
    async fn add_client_secret(
        &self,
        id: &ObjectId,
        secret: impl Into<String> + Send,
    ) -> Result<()>;
    async fn get_info_of_users(&self) -> Result<Vec<AuthInfo>>;
    /// 修改指定用户角色（仅管理员可调用，授权在路由层校验）。
    async fn set_user_role(&self, id: &ObjectId, role: Role) -> Result<()>;
}

/// `users` / `users_client_secrets` / `users_info` 的 DDL 已收敛到
/// [`shared::database::dashboard_schema`]，使 gateway 与 dashboard 执行同一份迁移
/// （gateway 先启动也能把表建好）。这里不再保留第二份 SQL，避免两处漂移。
const _: () = ();

#[async_trait::async_trait]
impl Authentication for Database {
    async fn init_authentication(&self, tx: &mut Transaction<'_, Postgres>) -> Result<()> {
        // 初始化用户表（迁移入口会在 advisory lock 保护下调用）。
        // DDL 本体在 shared 里，保证两个进程迁移结果一致。
        shared::database::dashboard_schema::initialize_users(tx).await?;
        Ok(())
    }

    async fn ensure_default_admin(&self) -> Result<()> {
        // 检查是否存在用户，若无则创建默认管理员
        match self.get_first_user().await {
            Ok(_) => Ok(()),
            Err(_) => {
                event!(
                    Level::INFO,
                    "No users found, creating default admin with role=admin"
                );
                let secret = generate_random_secret();
                self.create_user(DEFAULT_ADMIN_USERNAME.as_str(), &secret, Role::Admin)
                    .await?;
                Ok(())
            }
        }
    }

    async fn create_user(
        &self,
        username: impl Into<String> + Send,
        totp_secret: &str,
        role: Role,
    ) -> Result<DatabaseAuthentication> {
        let username = username.into();
        let id = ObjectId::new();
        let jwt_secret = generate_random_secret();

        // 直接插入，依赖数据库唯一索引保证不重复
        let result = sqlx::query(
            r#"
            INSERT INTO users (id, username, totp_secret, jwt_secret, role)
            VALUES ($1, $2, $3, $4, $5)
            "#,
        )
        .bind(id)
        .bind(&username)
        .bind(totp_secret)
        .bind(&jwt_secret)
        .bind(role.as_str())
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
        let row = sqlx::query(
            r#"
            SELECT * FROM users_info
            WHERE LOWER(username) = LOWER($1)
            "#,
        )
        .bind(username)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| anyhow!("User '{}' not found", username))?;

        Ok(DatabaseAuthentication::from_row(&row)?)
    }

    async fn get_first_user(&self) -> Result<DatabaseAuthentication> {
        let row = sqlx::query(
            r#"
            SELECT * FROM users_info
            ORDER BY created_at ASC
            LIMIT 1
            "#,
        )
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
            Err(e) => {
                return {
                    event!(Level::ERROR, "Failed to get user '{}': {}", username, e);
                    Ok(false)
                };
            } // 用户不存在视为验证失败
        };

        // lianjie
        let client_secrets = self.get_user_all_secrets(&user.id).await?;
        let mut secrets = vec![user.totp_secret.clone()];
        if auth.verify_type == AuthVerifyTOTPType::Login {
            secrets.extend(client_secrets);
        }
        for secret in secrets {
            if get_totp_code(username, secret)?.eq(totp) {
                get_database()
                    .add_web_log(
                        &user.id,
                        &crate::models::log::LogContent::Raw(
                            match auth.verify_type {
                                AuthVerifyTOTPType::Login => "auth.user.login.success",
                                AuthVerifyTOTPType::WantBind => "auth.user.want_bind.success",
                            }
                            .to_string(),
                        ),
                        &addr,
                    )
                    .await?;
                return Ok(true);
            }
        }

        get_database()
            .add_web_log(
                &user.id,
                &crate::models::log::LogContent::Raw(
                    match auth.verify_type {
                        AuthVerifyTOTPType::Login => "auth.user.login.fail",
                        AuthVerifyTOTPType::WantBind => "auth.user.want_bind.fail",
                    }
                    .to_string(),
                ),
                &addr,
            )
            .await?;
        Ok(false)
    }

    async fn get_user_from_id(&self, user_id: &ObjectId) -> Result<DatabaseAuthentication> {
        let row = sqlx::query(
            r#"
            SELECT * FROM users_info
            WHERE id = $1
            "#,
        )
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

    async fn add_client_secret(
        &self,
        user_id: &ObjectId,
        secret: impl Into<String> + Send,
    ) -> Result<()> {
        let _ = sqlx::query(
            r#"
            INSERT INTO users_client_secrets (user_id, secret)
            VALUES ($1, $2)
            "#,
        )
        .bind(user_id)
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
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows)
    }

    async fn set_user_role(&self, id: &ObjectId, role: Role) -> Result<()> {
        let result = sqlx::query("UPDATE users SET role = $1 WHERE id = $2")
            .bind(role.as_str())
            .bind(id)
            .execute(&self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(anyhow!("User with id '{}' not found", id));
        }
        Ok(())
    }
}

/// 把初始化脚本拆成单条语句。
///
/// 必须逐条执行而不是 `sqlx::raw_sql`：`RawSql` 在 `Executor` 上引入 `'q` 借用参数，
/// 与 `Transaction` 的 reborrow 组合会触发 "implementation of `Executor` is not
/// general enough" 编译错误。

/// 授权模型的集成测试。
///
/// 需要真实数据库：设置 `DATABASE_URL` 后运行
/// `cargo test -p dashboard --lib authz -- --nocapture`。
/// 未设置 `DATABASE_URL` 时自动跳过，因此不会影响无数据库的 CI。
#[cfg(test)]
mod authz_tests {
    use super::*;
    use crate::models::auth::{Authorizer, Role};
    use crate::router::website;
    use axum::extract::FromRequestParts;
    use shared::models::websites::{
        CreateDatabaseWebsite, DatabaseWebsiteConfig, DatabaseWebsiteRequestIp,
    };

    fn database_url() -> Option<String> {
        std::env::var("DATABASE_URL").ok().filter(|v| !v.is_empty())
    }

    /// 进程内只需要初始化一次（`DATABASE` 是 `OnceLock`，重复 set 会失败）。
    static INIT: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

    /// 建库连接 + 确保 users 表存在。返回 None 表示跳过（没有 DATABASE_URL）。
    async fn setup() -> Option<()> {
        let url = database_url()?;
        INIT.get_or_init(|| async {
            // `sign_jwt` 通过 `get_config().token_exp` 读有效期，必须先初始化配置。
            let _ = crate::config::init_config();
            shared::database::init_database_with_mode(
                &url,
                5,
                shared::database::DbStartupMode::AutoMigrate,
            )
            .await
            .expect("init database");
            // 控制面自己的表不在 shared 的迁移里，这里单独建一次。
            let mut tx = get_database().pool.begin().await.expect("begin");
            sqlx::query("SELECT pg_advisory_xact_lock($1)")
                .bind(shared::database::locks::SCHEMA_INIT)
                .execute(&mut *tx)
                .await
                .expect("lock");
            get_database()
                .init_authentication(&mut tx)
                .await
                .expect("init authentication");
            tx.commit().await.expect("commit");
        })
        .await;
        Some(())
    }

    async fn upsert_user(username: &str, role: Role) -> ObjectId {
        // 先删掉同名用户，保证测试可重复运行。
        sqlx::query("DELETE FROM users WHERE LOWER(username) = LOWER($1)")
            .bind(username)
            .execute(&get_database().pool)
            .await
            .expect("delete existing test user");
        let user = get_database()
            .create_user(username, "JBSWY3DPEHPK3PXP", role)
            .await
            .expect("create test user");
        assert_eq!(user.role, role, "create_user 应写入指定角色");
        user.id
    }

    /// 授权链路的集成测试。
    ///
    /// 注意：刻意把多个断言放在**同一个** `#[tokio::test]` 里。
    /// `shared::database::DATABASE` 是进程级 `OnceLock`，其连接池由创建它的
    /// tokio runtime 驱动；libtest 会为每个 `#[tokio::test]` 建立独立 runtime，
    /// 跨 runtime 复用同一个池会 `PoolTimedOut`。
    #[tokio::test]
    async fn authorization_chain() {
        jwt_roundtrip().await;
        unknown_role_falls_back_to_view().await;
        authorizer_extracts_role_from_bearer_header().await;
        create_website_handler_enforces_roles().await;
        user_management_is_admin_only().await;
    }

    async fn cleanup(username: &str) {
        sqlx::query("DELETE FROM users WHERE LOWER(username) = LOWER($1)")
            .bind(username)
            .execute(&get_database().pool)
            .await
            .expect("cleanup");
    }

    /// 为指定用户建号并签发真实 JWT（供 handler 级测试使用）。
    async fn token_for(username: &str, role: Role) -> String {
        sqlx::query("DELETE FROM users WHERE LOWER(username) = LOWER($1)")
            .bind(username)
            .execute(&get_database().pool)
            .await
            .expect("delete existing");
        get_database()
            .create_user(username, "JBSWY3DPEHPK3PXP", role)
            .await
            .expect("create user");
        crate::auth::sign_jwt(username)
            .await
            .expect("sign_jwt")
            .token
    }

    /// 构造 handler 所需的最小建站请求体。
    fn create_body(host: &str) -> CreateDatabaseWebsite {
        CreateDatabaseWebsite {
            name: Some("authz-test".to_string()),
            hosts: vec![host.to_string()],
            ports: vec![18099],
            certificates: vec![],
            backends: vec![],
            config: Some(DatabaseWebsiteConfig {
                get_request_ip: DatabaseWebsiteRequestIp::Raw,
            }),
        }
    }

    /// 用真实 `sign_jwt` 生成令牌，再走完整的「解令牌 → 查库 → 授权」链路。
    async fn jwt_roundtrip() {
        if setup().await.is_none() {
            eprintln!("跳过：未设置 DATABASE_URL");
            return;
        }

        // --- view：只读 ---
        let view_id = upsert_user("__authz_view__", Role::View).await;
        let token = crate::auth::sign_jwt("__authz_view__")
            .await
            .expect("sign_jwt");
        let info = crate::auth::get_user_info_from_verify_jwt(&token.token)
            .await
            .expect("verify jwt");
        assert_eq!(
            info.user.role,
            Role::View,
            "JWT 校验后角色必须仍是 view（从 users_info.role 读取）"
        );
        assert_eq!(info.user.id, view_id);
        let auth = Authorizer(info);
        assert!(auth.require_write().is_err(), "view 角色不得通过写操作校验");
        assert!(auth.require_admin().is_err(), "view 角色不得通过管理员校验");

        // --- user：可写、不可管理 ---
        upsert_user("__authz_user__", Role::User).await;
        let token = crate::auth::sign_jwt("__authz_user__")
            .await
            .expect("sign_jwt");
        let info = crate::auth::get_user_info_from_verify_jwt(&token.token)
            .await
            .expect("verify jwt");
        assert_eq!(info.user.role, Role::User);
        let auth = Authorizer(info);
        assert!(auth.require_write().is_ok(), "user 角色应可通过写校验");
        assert!(
            auth.require_admin().is_err(),
            "user 角色不得通过管理员校验（不能改角色/枚举账号）"
        );

        // --- admin：全通过 ---
        upsert_user("__authz_admin__", Role::Admin).await;
        let token = crate::auth::sign_jwt("__authz_admin__")
            .await
            .expect("sign_jwt");
        let info = crate::auth::get_user_info_from_verify_jwt(&token.token)
            .await
            .expect("verify jwt");
        assert_eq!(info.user.role, Role::Admin);
        let auth = Authorizer(info);
        assert!(auth.require_write().is_ok());
        assert!(auth.require_admin().is_ok());

        // 清理
        for name in ["__authz_view__", "__authz_user__", "__authz_admin__"] {
            sqlx::query("DELETE FROM users WHERE LOWER(username) = LOWER($1)")
                .bind(name)
                .execute(&get_database().pool)
                .await
                .expect("cleanup");
        }
    }

    async fn create_website_handler_enforces_roles() {
        if setup().await.is_none() {
            eprintln!("跳过：未设置 DATABASE_URL");
            return;
        }

        // view：必须 403
        let view = token_for("__h_view__", Role::View).await;
        let resp = website::create(
            Authorizer(
                crate::auth::get_user_info_from_verify_jwt(&view)
                    .await
                    .unwrap(),
            ),
            axum::Json(create_body("authz-view.test")),
        )
        .await;
        assert_eq!(
            resp.status(),
            403,
            "view 角色调用 POST /websites/create 必须返回 403"
        );

        // user：应通过授权检查（能走到实际创建逻辑）
        let user = token_for("__h_user__", Role::User).await;
        let resp = website::create(
            Authorizer(
                crate::auth::get_user_info_from_verify_jwt(&user)
                    .await
                    .unwrap(),
            ),
            axum::Json(create_body("authz-user.test")),
        )
        .await;
        assert_eq!(
            resp.status(),
            200,
            "user 角色调用 POST /websites/create 应成功，实际 {}",
            resp.status()
        );

        // 清理创建的站点与用户
        for host in ["authz-view.test", "authz-user.test"] {
            let _ = sqlx::query("DELETE FROM websites WHERE $1 = ANY(hosts)")
                .bind(host)
                .execute(&get_database().pool)
                .await;
        }
        cleanup("__h_view__").await;
        cleanup("__h_user__").await;
    }

    async fn user_management_is_admin_only() {
        if setup().await.is_none() {
            eprintln!("跳过：未设置 DATABASE_URL");
            return;
        }

        // view 调 all_users 应 403
        let view = token_for("__h_view2__", Role::View).await;
        let info = crate::auth::get_user_info_from_verify_jwt(&view)
            .await
            .unwrap();
        let resp = crate::auth::all_users(Authorizer(info)).await;
        assert_eq!(resp.status(), 403, "view 不得枚举账号");

        // user 调 all_users 应 403
        let user = token_for("__h_user2__", Role::User).await;
        let info = crate::auth::get_user_info_from_verify_jwt(&user)
            .await
            .unwrap();
        let resp = crate::auth::all_users(Authorizer(info)).await;
        assert_eq!(resp.status(), 403, "user 不得枚举账号");

        // admin 调 all_users 应 200
        let admin = token_for("__h_admin2__", Role::Admin).await;
        let info = crate::auth::get_user_info_from_verify_jwt(&admin)
            .await
            .unwrap();
        let resp = crate::auth::all_users(Authorizer(info)).await;
        assert_eq!(resp.status(), 200, "admin 应能枚举账号");

        for name in ["__h_view2__", "__h_user2__", "__h_admin2__"] {
            cleanup(name).await;
        }
    }
    /// 角色列解码：非法/未知值必须降级为 view，绝不能放权。
    async fn unknown_role_falls_back_to_view() {
        if setup().await.is_none() {
            eprintln!("跳过：未设置 DATABASE_URL");
            return;
        }
        // 直接绕过 CHECK 约束不可行（这正是约束存在的意义），
        // 因此这里验证解析函数本身的降级行为。
        assert_eq!(Role::parse("admin"), Role::Admin);
        assert_eq!(Role::parse("ADMIN"), Role::Admin);
        assert_eq!(Role::parse("user"), Role::User);
        assert_eq!(Role::parse("view"), Role::View);
        assert_eq!(Role::parse("superuser"), Role::View);
        assert_eq!(Role::parse(""), Role::View);
        assert_eq!(Role::parse("readonly"), Role::View);
    }

    /// `Authorizer` 提取器应能从请求头解析 Bearer 令牌并带出角色。
    async fn authorizer_extracts_role_from_bearer_header() {
        if setup().await.is_none() {
            eprintln!("跳过：未设置 DATABASE_URL");
            return;
        }
        upsert_user("__authz_bearer__", Role::Admin).await;
        let token = crate::auth::sign_jwt("__authz_bearer__")
            .await
            .expect("sign_jwt");

        let req = axum::http::Request::builder()
            .uri("/")
            .header("Authorization", format!("Bearer {}", token.token))
            .body(())
            .expect("build request");
        let (mut parts, _) = req.into_parts();
        let auth = Authorizer::from_request_parts(&mut parts, &())
            .await
            .expect("authorizer should accept a valid bearer token");
        assert_eq!(auth.role(), Role::Admin);
        assert!(auth.is_admin());

        // 没有令牌必须被拒绝
        let req = axum::http::Request::builder()
            .uri("/")
            .body(())
            .expect("build request");
        let (mut parts, _) = req.into_parts();
        assert!(
            Authorizer::from_request_parts(&mut parts, &())
                .await
                .is_err(),
            "缺少 Authorization 头必须被拒绝"
        );

        sqlx::query("DELETE FROM users WHERE LOWER(username) = LOWER($1)")
            .bind("__authz_bearer__")
            .execute(&get_database().pool)
            .await
            .expect("cleanup");
    }
}
