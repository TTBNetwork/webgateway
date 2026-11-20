import { clearSession, type SessionUser } from './session'
import { navigate } from './router'
import { notifyError } from './toast'

export interface TrafficPoint {
  bucket: string
  requests: number
}

export interface QpsPoint {
  bucket: string
  qps: number
}

export interface QpsResponse {
  current_time: string
  series: QpsPoint[]
}

export interface GeoNode {
  id: string
  name: string | null
  country: string | null
  city: string | null
  status: string
  online: boolean
}

export interface GeoSummary {
  configured: boolean
  days: number
  countries: Record<string, number>
  unknown: number
  nodes: GeoNode[]
}

export interface Summary {
  timezone_offset_minutes: number
  websites: number
  certificates: number
  gateways_online: number
  gateways_total: number
  requests_today: number
  responses_today: number
  e4xx_today: number
  e5xx_today: number
  uv_today: number
  unique_ips_today: number
  requests_size_today: string
  response_size_today: string
  traffic: TrafficPoint[]
}

export interface Website {
  id: string
  name: string | null
  hosts: string[]
  ports: number[]
  backends: unknown
  config: unknown
  created_at: string
  updated_at: string
}

export type CertificateStatus = 'pending' | 'valid' | 'expiring' | 'expired'

export interface Certificate {
  id: string
  name: string | null
  hostnames: string[]
  email: string | null
  dns_provider_id: string | null
  auto_renew: boolean
  dns_zone: string | null
  last_error: string | null
  status: CertificateStatus
  signing_started_at: string | null
  expires_at: string | null
  created_at: string
}

export interface DnsProvider {
  id: string
  name: string | null
  domains: string[]
  kind: string
  from_env: boolean
}

export interface UploadCertificatePayload {
  name?: string
  hostnames?: string[]
  fullchain: string
  private_key: string
  email?: string
}

export interface IssueCertificatePayload {
  name?: string
  hostnames: string[]
  email?: string
  dns_provider_id?: string
  dns_zone?: string
}

export interface UserSummary {
  id: string
  username: string
  role: string
  token_version: number
  last_login: string | null
  last_ip: string | null
  created_at: string
}

export interface CreateUserResult {
  user: UserSummary
  totp_secret: string
}

export interface WebsiteInput {
  name?: string
  hosts: string[]
  ports: number[]
  backends: unknown
  config: unknown
}

export interface Gateway {
  id: string
  name: string | null
  country: string | null
  city: string | null
  version: string | null
  status: string
  last_seen_at: string | null
  started_at: string | null
  online: boolean
}

export interface AccessLog {
  id: string
  host: string
  method: string
  path: string
  remote_addr: string
  http_version: string
  website_id: string | null
  request_headers: Record<string, unknown> | null
  response_headers: Record<string, unknown> | null
  request_size: string
  response_size: string | null
  requested_at: string
  status: number | null
}

export interface DnsProviderInput {
  name?: string
  domains: string[]
  kind?: string
  from_env: boolean
  secret_id?: string
  secret_key?: string
}

export interface AcmeProxy {
  url: string | null
}

export interface LoginResponse {
  user: SessionUser
  expires_in: number
}

interface Items<T> {
  items: T[]
}

async function errorText(response: Response): Promise<string> {
  try {
    const body = (await response.json()) as { error?: string }
    return body.error ?? 'HTTP ' + response.status
  } catch {
    return 'HTTP ' + response.status
  }
}

async function send<T>(path: string, method: string, body?: unknown): Promise<T> {
  const headers: Record<string, string> = {}
  if (body !== undefined) headers['Content-Type'] = 'application/json'

  // The session travels in an HttpOnly cookie, which the browser attaches to
  // same-origin requests automatically; no token is kept in JavaScript.
  const response = await fetch('/api/' + path, {
    method,
    headers,
    credentials: 'same-origin',
    cache: 'no-store',
    body: body === undefined ? undefined : JSON.stringify(body),
  })

  if (response.status === 401) {
    clearSession()
    navigate('/login')
    throw new Error('Unauthorized')
  }
  if (!response.ok) {
    const message = await errorText(response)
    notifyError(message)
    throw new Error(message)
  }

  return (await response.json()) as T
}

async function request<T>(path: string): Promise<T> {
  return send<T>(path, 'GET')
}

async function login(username: string, code: string): Promise<LoginResponse> {
  const response = await fetch('/api/auth/login', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    credentials: 'same-origin',
    body: JSON.stringify({ username, code }),
  })
  if (!response.ok) {
    throw new Error(await errorText(response))
  }
  return (await response.json()) as LoginResponse
}

export const api = {
  login,
  me: () => request<{ user: SessionUser }>('auth/me').then((value) => value.user),
  summary: () => {
    // Show "today" in the browser's timezone instead of the database UTC day.
    const offset = -new Date().getTimezoneOffset()
    return request<Summary>('summary?tz_offset_minutes=' + String(offset))
  },
  websites: () => request<Items<Website>>('websites').then((value) => value.items),
  geo: () => request<GeoSummary>('geo?days=1'),
  qps: () => request<QpsResponse>('qps'),
  certificates: () => request<Items<Certificate>>('certificates').then((value) => value.items),
  uploadCertificate: (payload: UploadCertificatePayload) =>
    send<{ id: string; hostnames: string[] }>('certificates', 'POST', payload),
  issueCertificate: (payload: IssueCertificatePayload) =>
    send<{ id: string; status: string }>('certificates/issue', 'POST', payload),
  reissueCertificate: (id: string) =>
    send<{ id: string; status: string }>(
      'certificates/' + encodeURIComponent(id) + '/issue',
      'POST',
    ),
  deleteCertificate: (id: string) =>
    send<{ deleted: boolean }>('certificates/' + encodeURIComponent(id), 'DELETE'),
  dnsProviders: () => request<Items<DnsProvider>>('dns-providers').then((value) => value.items),
  createDnsProvider: (payload: DnsProviderInput) =>
    send<{ id: string }>('dns-providers', 'POST', payload),
  deleteDnsProvider: (id: string) =>
    send<{ deleted: boolean }>('dns-providers/' + encodeURIComponent(id), 'DELETE'),
  getAcmeProxy: () => request<AcmeProxy>('settings/acme-proxy'),
  updateAcmeProxy: (url: string | null) => send<AcmeProxy>('settings/acme-proxy', 'PUT', { url }),
  users: () => request<Items<UserSummary>>('users').then((value) => value.items),
  createUser: (username: string, role: string) =>
    send<CreateUserResult>('users', 'POST', { username, role }),
  updateUser: (id: string, payload: { username?: string; role?: string }) =>
    send<{ user: UserSummary }>('users/' + encodeURIComponent(id), 'PATCH', payload),
  deleteUser: (id: string) =>
    send<{ deleted: boolean }>('users/' + encodeURIComponent(id), 'DELETE'),
  resetUserTotp: (id: string) =>
    send<{ totp_secret: string }>('users/' + encodeURIComponent(id) + '/totp', 'POST'),
  revokeUser: (id: string) =>
    send<{ revoked: boolean }>('users/' + encodeURIComponent(id) + '/revoke', 'POST'),
  logout: () => send<{ status: string }>('auth/logout', 'POST'),
  createWebsite: (payload: WebsiteInput) => send<{ id: string }>('websites', 'POST', payload),
  updateWebsite: (id: string, payload: WebsiteInput) =>
    send<{ updated: boolean }>('websites/' + encodeURIComponent(id), 'PUT', payload),
  deleteWebsite: (id: string) =>
    send<{ deleted: boolean }>('websites/' + encodeURIComponent(id), 'DELETE'),
  updateGateway: (
    id: string,
    payload: { name?: string; country?: string; city?: string; address?: string },
  ) => send<{ updated: boolean }>('gateways/' + encodeURIComponent(id), 'PATCH', payload),
  deleteGateway: (id: string) =>
    send<{ deleted: boolean }>('gateways/' + encodeURIComponent(id), 'DELETE'),
  gateways: () => request<Items<Gateway>>('gateways').then((value) => value.items),
  logs: (limit: number) =>
    request<Items<AccessLog>>('logs?limit=' + String(limit)).then((value) => value.items),
}
