<script setup lang="ts">
import { onMounted, onUnmounted, ref } from 'vue'
import QpsChart from './QpsChart.vue'
import { api, type QpsPoint } from '../api'
import { errorMessage } from '../format'
import { t } from '../i18n'

const INTERVAL_MS = 5000
const OFFSET_MS = 2500
const SLOT_COUNT = 60

const points = ref<QpsPoint[]>([])
const latest = ref(0)
const error = ref('')
let timer: number | undefined
let refreshing = false

function align(series: QpsPoint[], currentTime: string): void {
  const clock = Date.parse(currentTime)
  const end = Number.isFinite(clock)
    ? Math.floor(clock / INTERVAL_MS) * INTERVAL_MS - INTERVAL_MS
    : Math.floor(Date.now() / INTERVAL_MS) * INTERVAL_MS - INTERVAL_MS
  const values = new Map<number, number>()
  for (const point of series) {
    const time = Date.parse(point.bucket)
    if (Number.isFinite(time)) values.set(time, point.qps)
  }
  const aligned = Array.from({ length: SLOT_COUNT }, (_, index) => {
    const time = end - (SLOT_COUNT - 1 - index) * INTERVAL_MS
    return { bucket: new Date(time).toISOString(), qps: values.get(time) ?? 0 }
  })
  points.value = aligned
  latest.value = aligned.at(-1)?.qps ?? 0
}

async function refresh(): Promise<void> {
  if (refreshing) return
  refreshing = true
  try {
    const response = await api.qps()
    align(response.series, response.current_time)
    error.value = ''
  } catch (reason) {
    error.value = errorMessage(reason)
  } finally {
    refreshing = false
  }
}

// v1 refresh loop: poll on browser wall-clock boundaries at :02.5, :07.5,
// :12.5, ... and render the last completed 5s buckets. It deliberately does
// not refresh the rest of the dashboard.
function nextBoundaryDelay(): number {
  const now = Date.now()
  const nextBoundary = Math.ceil((now - OFFSET_MS) / INTERVAL_MS) * INTERVAL_MS + OFFSET_MS
  return Math.max(0, nextBoundary - now)
}

function schedule(): void {
  if (timer !== undefined) window.clearTimeout(timer)
  if (document.visibilityState !== 'visible') {
    timer = undefined
    return
  }
  timer = window.setTimeout(async () => {
    if (document.visibilityState === 'visible') await refresh()
    schedule()
  }, nextBoundaryDelay())
}

function onVisibility(): void {
  if (document.visibilityState === 'visible') {
    void refresh()
    schedule()
  } else if (timer !== undefined) {
    window.clearTimeout(timer)
    timer = undefined
  }
}

onMounted(() => {
  void refresh()
  schedule()
  document.addEventListener('visibilitychange', onVisibility)
})

onUnmounted(() => {
  if (timer !== undefined) window.clearTimeout(timer)
  document.removeEventListener('visibilitychange', onVisibility)
})
</script>

<template>
  <div class="card qps-card">
    <div class="card-header">
      <div>
        <h3>{{ t('dashboard.qpsTitle') }}</h3>
        <p>{{ t('dashboard.qpsSubtitle') }}</p>
      </div>
      <span class="badge badge-muted">{{ latest.toFixed(2) }} QPS</span>
    </div>
    <QpsChart :points="points" />
    <p v-if="error" class="form-error qps-error">{{ error }}</p>
  </div>
</template>
