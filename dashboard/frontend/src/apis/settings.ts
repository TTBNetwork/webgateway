import { gotWithAuth } from '../constant';
import type { APIResponse } from '../types';
import type { AccessLogRetention } from '../types/settings';

const prefix = 'settings';

/** 读取访问日志保留期配置。 */
export async function get_retention(): Promise<AccessLogRetention> {
    const resp = (await (
        await gotWithAuth.get(`${prefix}/retention`)
    ).json()) as APIResponse<AccessLogRetention>;
    if (resp.status !== 200 || !resp.data) {
        throw new Error(resp.message || `获取保留期配置失败 (${resp.status})`);
    }
    return resp.data;
}

/**
 * 更新保留期配置。
 *
 * 注意：后端会把低于下限（90 天）的值**夹紧**后返回实际生效值，
 * 调用方必须用返回值回显，否则界面会显示一个并未生效的数字。
 */
export async function set_retention(
    retention_days: number,
    enabled: boolean,
): Promise<AccessLogRetention> {
    const resp = (await (
        await gotWithAuth.post(`${prefix}/retention`, {
            json: { retention_days, enabled },
        })
    ).json()) as APIResponse<AccessLogRetention>;
    if (resp.status !== 200 || !resp.data) {
        throw new Error(resp.message || `保存保留期配置失败 (${resp.status})`);
    }
    return resp.data;
}

/** 立即执行一轮清理，返回本次删除的行数。 */
export async function prune_now(): Promise<number> {
    const resp = (await (
        await gotWithAuth.post(`${prefix}/retention/prune`)
    ).json()) as APIResponse<number>;
    if (resp.status !== 200 || resp.data === null) {
        throw new Error(resp.message || `清理失败 (${resp.status})`);
    }
    return resp.data;
}
