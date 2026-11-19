import { reactive } from 'vue'

export interface SessionUser {
  id: string
  username: string
  role: string
}

const TOKEN_KEY = 'wg.token'
const USER_KEY = 'wg.user'

function readToken(): string {
  try {
    return localStorage.getItem(TOKEN_KEY) ?? ''
  } catch {
    return ''
  }
}

function readUser(): SessionUser | null {
  try {
    const raw = localStorage.getItem(USER_KEY)
    if (!raw) return null
    const parsed = JSON.parse(raw) as Partial<SessionUser>
    if (
      typeof parsed.id === 'string' &&
      typeof parsed.username === 'string' &&
      typeof parsed.role === 'string'
    ) {
      return { id: parsed.id, username: parsed.username, role: parsed.role }
    }
    return null
  } catch {
    return null
  }
}

export const session = reactive<{ token: string; user: SessionUser | null }>({
  token: readToken(),
  user: readUser(),
})

export function setSession(token: string, user: SessionUser): void {
  session.token = token
  session.user = user
  try {
    localStorage.setItem(TOKEN_KEY, token)
    localStorage.setItem(USER_KEY, JSON.stringify(user))
  } catch {
    // Storage can be unavailable in private mode; the in-memory session still works.
  }
}

export function clearToken(): void {
  session.token = ''
  session.user = null
  try {
    localStorage.removeItem(TOKEN_KEY)
    localStorage.removeItem(USER_KEY)
  } catch {
    // Ignore storage errors.
  }
}
