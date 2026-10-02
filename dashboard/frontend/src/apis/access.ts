import { gotWithAuth } from '../constant';
import type { APIResponse } from '../types';
import type {
    AccessInfo,
    MapType,
    ResponseQPS,
    TodayMetricsInfoOfWebsites,
} from '../types/access';

const prefix = 'access';

export async function get_qps(interval: number = 5, count: number = 60) {
    const resp = (await (
        await gotWithAuth.get(`${prefix}/qps`, {
            searchParams: {
                interval,
                count,
            },
        })
    ).json()) as APIResponse<ResponseQPS>;
    return resp;
}

export async function get_access_info(
    in_days: number = 1,
    signal?: AbortSignal,
) {
    const resp = (await (
        await gotWithAuth.get(`${prefix}/info`, {
            searchParams: {
                in_days,
            },
            signal,
        })
    ).json()) as APIResponse<AccessInfo>;
    if (resp.status !== 200 || !resp.data) {
        throw new Error(resp.message || `获取访问统计失败 (${resp.status})`);
    }
    return resp.data;
}

export async function get_today_metrics_info_of_websites() {
    const resp = (await (
        await gotWithAuth.get(`${prefix}/metrics/websites`)
    ).json()) as APIResponse<TodayMetricsInfoOfWebsites[]>;
    return resp.data;
}

export async function get_access_map(
    in_days: number = 1,
    type: MapType = 'global',
    signal?: AbortSignal,
) {
    const resp = (await (
        await gotWithAuth.get(`${prefix}/access_map`, {
            searchParams: {
                in_days,
                type,
            },
            signal,
        })
    ).json()) as APIResponse<Record<string, number>>;
    if (resp.status !== 200 || !resp.data) {
        throw new Error(resp.message || `获取访问地图失败 (${resp.status})`);
    }
    return resp.data;
}
