use serde::{Deserialize, Serialize};

/// 访问日志保留期配置，存放在 `configurations` 表中（key = `access_log_retention`）。
///
/// 背景：访问日志表**没有分区、也没有 TTL**，会无限增长。生产库在 2026-10-02 已是
/// 6.3 GB（其中 `access_response_size_logs` 单表 3.6 GB / 1556 万行），而主机磁盘
/// 只剩 19 GB。这里提供"只保留最近 N 天"的策略，由后台任务按批删除历史数据。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccessLogRetention {
    /// 保留天数。**下限 90 天（约 3 个月）**，上限 3650 天（10 年，等于事实上不删）。
    pub retention_days: u32,
    /// 是否启用自动清理。默认开启 —— 表的无限增长是当前最大的容量风险。
    pub enabled: bool,
}

impl AccessLogRetention {
    /// 保留期下限：3 个月（按 90 天计）。低于此值的一律夹紧到 90。
    pub const MIN_RETENTION_DAYS: u32 = 90;
    /// 保留期上限：10 年，等价于"不清理"。
    pub const MAX_RETENTION_DAYS: u32 = 3650;
    /// 默认保留 6 个月。
    pub const DEFAULT_RETENTION_DAYS: u32 = 180;
    /// `configurations` 表中的键名。
    pub const CONFIG_KEY: &'static str = "access_log_retention";

    /// 把任意外部输入规范化为合法配置（夹紧上下限）。
    pub fn sanitized(retention_days: u32, enabled: bool) -> Self {
        Self {
            retention_days: retention_days.clamp(Self::MIN_RETENTION_DAYS, Self::MAX_RETENTION_DAYS),
            enabled,
        }
    }

    /// 该保留期是否需要真正执行删除（等于上限说明用户想永久保留）。
    pub fn should_prune(&self) -> bool {
        self.enabled && self.retention_days < Self::MAX_RETENTION_DAYS
    }
}

impl Default for AccessLogRetention {
    fn default() -> Self {
        Self {
            retention_days: Self::DEFAULT_RETENTION_DAYS,
            enabled: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_clamps_to_allowed_range() {
        assert_eq!(
            AccessLogRetention::sanitized(0, true).retention_days,
            AccessLogRetention::MIN_RETENTION_DAYS,
            "低于下限应被夹紧到 90 天（约 3 个月）"
        );
        assert_eq!(
            AccessLogRetention::sanitized(30, true).retention_days,
            AccessLogRetention::MIN_RETENTION_DAYS
        );
        assert_eq!(
            AccessLogRetention::sanitized(365, true).retention_days,
            365,
            "区间内应原样保留"
        );
        assert_eq!(
            AccessLogRetention::sanitized(u32::MAX, true).retention_days,
            AccessLogRetention::MAX_RETENTION_DAYS
        );
    }

    #[test]
    fn prune_requires_enabled_and_finite_retention() {
        assert!(AccessLogRetention::default().should_prune());
        assert!(!AccessLogRetention::sanitized(180, false).should_prune());
        assert!(
            !AccessLogRetention::sanitized(AccessLogRetention::MAX_RETENTION_DAYS, true)
                .should_prune(),
            "上限等价于永久保留，不应触发删除"
        );
    }
}
