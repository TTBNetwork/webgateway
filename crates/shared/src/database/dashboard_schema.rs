//! 控制面（dashboard）特有的表：`users` / `users_client_secrets` / `web_log`。
//!
//! 为什么放在 `shared` 而不是 dashboard 里：需求是「gateway 或 dashboard 任一先启动都能
//! 自动、无感地完成迁移」。原先这三张表的 DDL 在 dashboard crate 内，gateway 无法调用，
//! 于是「gateway 先启动」时库里就缺 `users`（而 gateway 的
//! `verify_database_schema` 恰恰要求它存在）—— 只能靠"先起 dashboard"这种隐性顺序。
//! 把 DDL 收敛到共享迁移入口后，两个进程执行的是**完全相同的迁移**，谁先启动都一样。
//!
//! dashboard crate 的 `init_authentication` 现在只是转调本模块，保持既有调用点不变。

use sqlx::{Postgres, Transaction};

/// `users` / `users_client_secrets` / `users_info` 视图的 DDL。
///
/// 注意 `users_info` 必须包含 `role`（`SELECT * FROM users_info` 要能取到角色），
/// 且必须先 `DROP VIEW` 再 `CREATE VIEW`：`CREATE OR REPLACE VIEW` 只允许在末尾追加列，
/// 在中间插入 `role` 会报 `cannot change name of view column "created_at" to "role"`。
pub const USERS_INIT_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS users (
    id TEXT PRIMARY KEY,
    username TEXT NOT NULL,
    totp_secret TEXT NOT NULL,
    jwt_secret TEXT NOT NULL,
    role TEXT NOT NULL DEFAULT 'view',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_login TIMESTAMPTZ,
    last_ip TEXT,
    addresses TEXT[] NOT NULL DEFAULT '{}',
    UNIQUE (username)
);

ALTER TABLE users ADD COLUMN IF NOT EXISTS role TEXT;
-- 已存在的用户按管理员处理，避免升级后把现有使用者锁在门外。
UPDATE users SET role = 'admin' WHERE role IS NULL;
ALTER TABLE users ALTER COLUMN role SET DEFAULT 'view';
ALTER TABLE users ALTER COLUMN role SET NOT NULL;
ALTER TABLE users DROP CONSTRAINT IF EXISTS users_role_check;
ALTER TABLE users ADD CONSTRAINT users_role_check CHECK (role IN ('admin', 'user', 'view'));

CREATE TABLE IF NOT EXISTS users_client_secrets (
    user_id TEXT NOT NULL REFERENCES users (id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    secret TEXT NOT NULL,
    UNIQUE (user_id, secret)
);

CREATE UNIQUE INDEX IF NOT EXISTS users_username_idx ON users (LOWER(username));
CREATE INDEX IF NOT EXISTS users_client_secrets_user_id_idx ON users_client_secrets (user_id);
CREATE INDEX IF NOT EXISTS users_client_secrets_secret_idx ON users_client_secrets (secret);

DROP VIEW IF EXISTS users_info;
CREATE VIEW users_info AS
SELECT
    id, username, totp_secret, jwt_secret, role,
    created_at, updated_at, last_login, last_ip, addresses,
    (SELECT COUNT(*) FROM users_client_secrets WHERE user_id = users.id) AS client_secrets_count,
    EXISTS (SELECT 1 FROM users_client_secrets cs WHERE cs.user_id = users.id) AS bound_totp
FROM users;
"#;

/// 创建 `users` 相关对象。调用方必须已持有 `SCHEMA_INIT` 锁。
pub async fn initialize_users(tx: &mut Transaction<'_, Postgres>) -> anyhow::Result<()> {
    for statement in split_sql_statements(USERS_INIT_SQL) {
        sqlx::query(&statement).execute(&mut **tx).await?;
    }
    Ok(())
}

/// 创建 `web_log` 表（依赖 `users`，必须在其之后执行）。
pub async fn initialize_web_log(tx: &mut Transaction<'_, Postgres>) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS web_log (
            id TEXT PRIMARY KEY,
            user_id TEXT NOT NULL,
            content JSONB NOT NULL default '{}',
            created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            address TEXT NOT NULL,
            FOREIGN KEY (user_id) REFERENCES users(id)
        )
    "#,
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// 把 DDL 脚本拆成单条语句（按行累积、遇到以 `;` 结尾的行即切分；注释与空行跳过）。
///
/// 这段 SQL 里没有字符串字面量中的分号，也没有 `$$ ... $$` 函数体，因此简单切分即可。
pub fn split_sql_statements(sql: &str) -> Vec<String> {
    let mut statements = Vec::new();
    let mut current = String::new();
    for line in sql.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("--") {
            continue;
        }
        current.push_str(line);
        current.push('\n');
        if trimmed.ends_with(';') {
            statements.push(std::mem::take(&mut current));
        }
    }
    if !current.trim().is_empty() {
        statements.push(current);
    }
    statements
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_statements_and_keeps_view_recreation() {
        let stmts = split_sql_statements(USERS_INIT_SQL);
        assert!(
            stmts.iter().any(|s| s.contains("DROP VIEW IF EXISTS users_info")),
            "必须保留 DROP VIEW —— CREATE OR REPLACE VIEW 无法在中间插列"
        );
        assert!(
            stmts.iter().any(|s| s.contains("CREATE VIEW users_info")),
            "必须重建 users_info 视图"
        );
        assert!(
            !stmts.iter().any(|s| s.starts_with("--")),
            "注释行不应成为独立语句"
        );
    }
}
