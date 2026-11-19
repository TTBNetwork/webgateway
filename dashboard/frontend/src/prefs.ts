import { reactive, watchEffect } from 'vue'

export type Theme = 'dark' | 'light'
export type Locale = 'zh' | 'en'

function readStorage(key: string): string | null {
  try {
    return localStorage.getItem(key)
  } catch {
    return null
  }
}

function writeStorage(key: string, value: string): void {
  try {
    localStorage.setItem(key, value)
  } catch {
    // Ignore storage errors.
  }
}

function initialTheme(): Theme {
  return readStorage('wg.theme') === 'light' ? 'light' : 'dark'
}

function initialLocale(): Locale {
  return readStorage('wg.locale') === 'en' ? 'en' : 'zh'
}

export const prefs = reactive({
  theme: initialTheme(),
  locale: initialLocale(),
})

watchEffect(() => {
  document.documentElement.dataset.theme = prefs.theme
  document.documentElement.lang = prefs.locale === 'zh' ? 'zh-CN' : 'en'
  writeStorage('wg.theme', prefs.theme)
  writeStorage('wg.locale', prefs.locale)
})
