import { reactive } from 'vue'

export interface SessionUser {
  id: string
  username: string
  role: string
}

/**
 * The session JWT is stored in an HttpOnly cookie by the API, so scripts cannot
 * read it. The panel only keeps the authenticated user in memory and restores
 * it with GET /api/auth/me on boot.
 */
export const session = reactive<{ user: SessionUser | null; ready: boolean }>({
  user: null,
  ready: false,
})

export function setSession(user: SessionUser): void {
  session.user = user
  session.ready = true
}

export function clearSession(): void {
  session.user = null
  session.ready = true
}
