import { reactive } from 'vue'

export type ToastKind = 'success' | 'error' | 'info'

export interface Toast {
  id: number
  kind: ToastKind
  message: string
}

export const toasts = reactive<Toast[]>([])
let nextId = 1

export function dismissToast(id: number): void {
  const index = toasts.findIndex((toast) => toast.id === id)
  if (index >= 0) toasts.splice(index, 1)
}

export function notify(kind: ToastKind, message: string, timeout = 4500): void {
  const id = nextId++
  toasts.push({ id, kind, message })
  if (toasts.length > 4) toasts.shift()
  if (timeout > 0) {
    window.setTimeout(() => dismissToast(id), timeout)
  }
}

export function notifySuccess(message: string): void {
  notify('success', message)
}

export function notifyError(message: string): void {
  notify('error', message, 6500)
}

export function notifyInfo(message: string): void {
  notify('info', message)
}
