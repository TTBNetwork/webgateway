import { clearToken, session } from './session'
import { navigate } from './router'

export interface TrafficPoint {
  bucket: string
  requests: number
}

export interface Summary {
  websites: number
  certificates: number
  gateways_online: number
  gateways_total: number
  requests_today: number
  responses_today: number
  e4xx_today: number
  e5xx_today: number
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

export interface Certificate {
  id: string
  name: string | null
  hostnames: string[]
  email: string | null
  dns_provider_id: string | null
  expires_at: string | null
  created_at: string
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
  request_size: string
  response_size: string | null
  requested_at: string
  status: number | null
}

interface Items<T> {
  items: T[]
}

async function request<T>(path: string): Promise<T> {
  const headers: Record<string, string> = {}
  if (session.token) headers.Authorization = 'Bearer ' + session.token

  const response = await fetch('/api/' + path, {
    headers,
    cache: 'no-store',
  })

  if (response.status === 401) {
    clearToken()
    navigate('/login')
    throw new Error('Unauthorized')
  }
  if (!response.ok) {
    let message = 'HTTP ' + response.status
    try {
      const body = (await response.json()) as { error?: string }
      if (body.error) message = body.error
    } catch {
      // Non-JSON error body; keep the status message.
    }
    throw new Error(message)
  }

  return (await response.json()) as T
}

export const api = {
  summary: () => request<Summary>('summary'),
  websites: () => request<Items<Website>>('websites').then((value) => value.items),
  certificates: () => request<Items<Certificate>>('certificates').then((value) => value.items),
  gateways: () => request<Items<Gateway>>('gateways').then((value) => value.items),
  logs: (limit: number) =>
    request<Items<AccessLog>>('logs?limit=' + String(limit)).then((value) => value.items),
}
