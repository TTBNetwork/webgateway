import { prefs } from './prefs'

function locale(): string {
  return prefs.locale === 'en' ? 'en-US' : 'zh-CN'
}

export function formatNumber(value: number | string | null | undefined): string {
  const number = Number(value ?? 0)
  return new Intl.NumberFormat(locale()).format(Number.isFinite(number) ? number : 0)
}

export function formatBytes(value: number | string | null | undefined): string {
  let bytes = Number(value ?? 0)
  if (!Number.isFinite(bytes) || bytes < 0) bytes = 0
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  let unit = 0
  while (bytes >= 1024 && unit < units.length - 1) {
    bytes /= 1024
    unit += 1
  }
  const digits = unit === 0 ? 0 : bytes >= 10 ? 1 : 2
  return bytes.toFixed(digits) + ' ' + units[unit]
}

export function formatDate(value: string | null | undefined): string {
  if (!value) return '—'
  const date = new Date(value)
  if (Number.isNaN(date.getTime())) return '—'
  return new Intl.DateTimeFormat(locale(), {
    dateStyle: 'medium',
    timeStyle: 'short',
  }).format(date)
}

export function formatRelative(value: string | null | undefined): string {
  if (!value) return '—'
  const date = new Date(value)
  if (Number.isNaN(date.getTime())) return '—'
  const seconds = Math.round((date.getTime() - Date.now()) / 1000)
  const formatter = new Intl.RelativeTimeFormat(locale(), { numeric: 'auto' })
  const ranges: Array<[Intl.RelativeTimeFormatUnit, number]> = [
    ['second', 60],
    ['minute', 60],
    ['hour', 24],
    ['day', 30],
    ['month', 12],
    ['year', Number.POSITIVE_INFINITY],
  ]
  let valueSeconds = seconds
  for (const [unit, size] of ranges) {
    if (Math.abs(valueSeconds) < size || unit === 'year') {
      return formatter.format(Math.round(valueSeconds), unit)
    }
    valueSeconds /= size
  }
  return '—'
}

export function daysUntil(value: string | null | undefined): number | null {
  if (!value) return null
  const date = new Date(value)
  if (Number.isNaN(date.getTime())) return null
  return Math.ceil((date.getTime() - Date.now()) / 86_400_000)
}

export function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}
