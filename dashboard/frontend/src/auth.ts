import { got, router } from './constant';
import addPresentation from './plugins/presentation';
import type { APIResponse, AuthResponse, UserInfo } from './types';
import type { BindTotpResponse } from './types/auth';

export async function login(username: string, totp: string): Promise<boolean> {
    const response: APIResponse<AuthResponse> = await (
        await got.post('auth/login', {
            json: {
                totp,
                username,
            },
        })
    ).json();

    if (response.status == 401) {
        addPresentation(response.message || '', 'alert');
        throw new Error('Invalid credentials');
    }
    localStorage.setItem('token', JSON.stringify(response.data));
    return true;
}

export function getLocalToken(): AuthResponse | null {
    const data = localStorage.getItem('token');
    try {
        const token: AuthResponse = data ? JSON.parse(data) : null;
        return +new Date(token?.exp_at) < +new Date() ? null : token;
    } catch (e) {
        return null;
    }
}

export function getToken(redirect: boolean = true): string | undefined {
    const token = getLocalToken();
    if (redirect && token == null) {
        addPresentation('Invaild Token', 'alert');
        router.push('/login');
        throw new Error('Invaild Token');
    }
    return token?.token;
}

export async function checkToken(): Promise<boolean> {
    if (getLocalToken() == null) {
        return false;
    }
    try {
        await info();
        return true;
    } catch (e) {
        localStorage.removeItem('token');
        return false;
    }
}

export async function info(): Promise<UserInfo> {
    const resp = (await (
        await got.get('auth', {
            headers: {
                Authorization: `Bearer ${getLocalToken()?.token}`,
            },
        })
    ).json()) as APIResponse<UserInfo>;
    if (resp.status == 200) {
        return resp.data;
    }
    throw new Error(resp.message);
}

export async function logout() {
    localStorage.removeItem('token');
    router.push('/login');
}

export async function getAllUsers() {
    return (await (
        await got.get('auth/users', {
            headers: {
                Authorization: `Bearer ${getLocalToken()?.token}`,
            },
        })
    ).json()) as APIResponse<UserInfo[]>;
}

export async function bindTotp(totp: string) {
    return (await (
        await got.post('auth/totp/qrcode/get', {
            headers: {
                Authorization: `Bearer ${getLocalToken()?.token}`,
            },
            json: {
                totp,
            },
        })
    ).json()) as APIResponse<BindTotpResponse>;
}

export async function refreshBindTotpQrcode(id: string) {
    return (await (
        await got.post('auth/totp/qrcode/refresh', {
            headers: {
                Authorization: `Bearer ${getLocalToken()?.token}`,
            },
            json: {
                secret_id: id,
            },
        })
    ).json()) as APIResponse<BindTotpResponse>;
}

export async function verifyBindTotp(totp: string, id: string) {
    return (await (
        await got.post('auth/totp/qrcode/verify', {
            headers: {
                Authorization: `Bearer ${getLocalToken()?.token}`,
            },
            json: {
                secret_id: id,
                totp,
            },
        })
    ).json()) as APIResponse<BindTotpResponse>;
}

/*route("/totp/qrcode/get", post(get_bind_qrcode))
        .route("/totp/qrcode/verify", post(verify_bind_qrcode))
        
        */
