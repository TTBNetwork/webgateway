/** 访问日志保留期配置（后端 `AccessLogRetention`）。 */
export interface AccessLogRetention {
    /** 保留天数，后端会夹紧到 [90, 3650]。 */
    retention_days: number;
    /** 是否启用自动清理。 */
    enabled: boolean;
}

/** 与后端常量保持一致，便于前端做即时校验提示。 */
export const MIN_RETENTION_DAYS = 90;
export const MAX_RETENTION_DAYS = 3650;
export const DEFAULT_RETENTION_DAYS = 180;
