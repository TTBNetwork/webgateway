import { gotWithAuth } from '../constant';
import type { APIResponse } from '../types';
import type { Website, WebsiteCreateRequest } from '../types/websites';

const prefix = 'websites';

export async function createWebsite(website: WebsiteCreateRequest) {
    const resp = (await (
        await gotWithAuth.post(`${prefix}/create`, {
            json: website,
        })
    ).json()) as APIResponse<Website>;
    return resp;
}

export async function getWebsites(): Promise<Website[]> {
    const resp = (await (
        await gotWithAuth.get(`${prefix}`)
    ).json()) as APIResponse<Website[]>;
    return resp.data;
}

export async function getWebsite(id: string): Promise<Website> {
    const resp = (await (
        await gotWithAuth.get(`${prefix}/${id}`)
    ).json()) as APIResponse<Website>;
    if (resp.status !== 200 || !resp.data) {
        throw new Error(resp.message || `获取站点失败 (${resp.status})`);
    }
    return resp.data;
}

/** 整条覆盖站点配置。后端对不存在的 id 返回 404，这里显式抛错以免前端误判成功。 */
export async function updateWebsite(id: string, website: WebsiteCreateRequest) {
    const resp = (await (
        await gotWithAuth.post(`${prefix}/${id}/update`, {
            json: website,
        })
    ).json()) as APIResponse<Website>;
    return resp;
}

/** 删除站点（后端要求 admin 角色）。 */
export async function deleteWebsite(id: string) {
    const resp = (await (
        await gotWithAuth.post(`${prefix}/${id}/delete`)
    ).json()) as APIResponse<boolean>;
    return resp;
}
