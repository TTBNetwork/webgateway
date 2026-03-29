export interface APIResponse<T = any> {
    status: number;
    message?: string;
    data: T;
}

export interface AuthResponse {
    token: string;
    exp_at: string;
}

export interface Log {
    id: string;
    user_id: string;
    content: LogContent;
    created_at: string;
    address: string;
}

export interface LogContent {
    type: string;
    content: string;
    params: LogContentParams[];
}

export interface LogContentParams {
    key: string;
    value: string;
}

export interface UserInfo {
    id: string;
    username: string;
    created_at: string;
    updated_at: string;
    bound_totp: boolean;
}
