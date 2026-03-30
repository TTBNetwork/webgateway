export type BindTotpState = 'input' | 'qrcode' | 'verified';

export interface BindTotpResponse {
    secret_id: string;
    qr_url: string;
}
