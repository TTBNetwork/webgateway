import { prefs } from './prefs'

export interface ChartTheme {
  dark: boolean
  accent: string
  accentStrong: string
  text: string
  muted: string
  panel: string
  border: string
  teal: string
  danger: string
  mapLand: string
  mapStroke: string
}

function cssValue(name: string, fallback: string): string {
  const value = getComputedStyle(document.documentElement).getPropertyValue(name).trim()
  return value || fallback
}

export function chartTheme(): ChartTheme {
  // Touch the reactive theme so computed chart options update on toggle.
  const dark = prefs.theme === 'dark'
  return {
    dark,
    accent: cssValue('--accent', dark ? '#f4d1b4' : '#0f8f88'),
    accentStrong: cssValue('--accent-strong', dark ? '#ebbb97' : '#0a756f'),
    text: cssValue('--text', dark ? '#f3eee5' : '#1d1d1f'),
    muted: cssValue('--muted', dark ? '#9a9994' : '#6e6e73'),
    panel: cssValue('--panel-solid', dark ? '#17181b' : '#ffffff'),
    border: cssValue('--border', dark ? 'rgba(255,255,255,0.08)' : 'rgba(0,0,0,0.1)'),
    teal: cssValue('--teal', '#0fc6c2'),
    danger: cssValue('--danger', '#ff5c6c'),
    mapLand: cssValue('--map-land', dark ? '#1d2023' : '#e5e5e5'),
    mapStroke: cssValue('--map-stroke', dark ? '#30343a' : '#d2d2d2'),
  }
}
