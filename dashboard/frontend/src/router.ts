import { reactive } from 'vue'

function normalize(path: string): string {
  const value = path.replace(/\/+$/, '')
  return value === '' ? '/' : value
}

export const route = reactive({ path: normalize(window.location.pathname) })

export function navigate(path: string): void {
  if (route.path === path) return
  window.history.pushState({}, '', path)
  route.path = normalize(path)
  window.scrollTo({ top: 0 })
}

window.addEventListener('popstate', () => {
  route.path = normalize(window.location.pathname)
})
