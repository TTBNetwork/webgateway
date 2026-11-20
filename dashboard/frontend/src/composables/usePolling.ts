import { onMounted, onUnmounted } from 'vue'

/**
 * Load once and refresh on an interval while the tab is visible.
 * Every page uses this instead of a manual refresh button.
 */
export function usePolling(load: () => Promise<void>, intervalMs = 15_000): void {
  let timer: number | undefined
  onMounted(() => {
    void load()
    timer = window.setInterval(() => {
      if (document.visibilityState === 'visible') void load()
    }, intervalMs)
  })
  onUnmounted(() => {
    if (timer) window.clearInterval(timer)
  })
}
