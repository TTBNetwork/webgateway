import { reactive } from 'vue'

const TOKEN_KEY = 'wg.token'

function initialToken(): string {
  try {
    return localStorage.getItem(TOKEN_KEY) ?? ''
  } catch {
    return ''
  }
}

export const session = reactive({ token: initialToken() })

export function setToken(token: string): void {
  session.token = token
  try {
    localStorage.setItem(TOKEN_KEY, token)
  } catch {
    // Storage can be unavailable in private mode; the in-memory token still works.
  }
}

export function clearToken(): void {
  session.token = ''
  try {
    localStorage.removeItem(TOKEN_KEY)
  } catch {
    // Ignore storage errors.
  }
}
