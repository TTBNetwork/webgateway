<script setup lang="ts">
import { computed } from 'vue'
import type { QpsPoint } from '../api'
import { prefs } from '../prefs'
import { t } from '../i18n'

const props = defineProps<{ points: QpsPoint[] }>()

const width = 520
const height = 170
const padding = { top: 10, right: 4, bottom: 20, left: 4 }

const SLOT_SECONDS = 5
const SLOT_COUNT = 60

// Keep a stable 60-slot window (5 minutes, one slot per 5 seconds), even when
// only one bucket has traffic. This prevents a single request from becoming
// one giant bar across the whole card.
const series = computed(() => {
  if (!props.points.length) return []
  const bySecond = new Map<number, number>()
  let latest = 0
  for (const point of props.points) {
    const second = Math.floor(Date.parse(point.bucket) / 1000)
    if (!Number.isFinite(second)) continue
    bySecond.set(second, point.qps)
    latest = Math.max(latest, second)
  }
  if (!latest) return []
  const end = Math.floor(Math.max(latest, Date.now() / 1000) / SLOT_SECONDS) * SLOT_SECONDS
  return Array.from({ length: SLOT_COUNT }, (_, index) => {
    const second = end - (SLOT_COUNT - 1 - index) * SLOT_SECONDS
    return {
      bucket: new Date(second * 1000).toISOString(),
      qps: bySecond.get(second) ?? 0,
    }
  })
})
const maxValue = computed(() => Math.max(0.01, ...series.value.map((point) => point.qps)))

const bars = computed(() => {
  const innerWidth = width - padding.left - padding.right
  const innerHeight = height - padding.top - padding.bottom
  const slot = series.value.length ? innerWidth / series.value.length : innerWidth
  return series.value.map((point, index) => {
    const barHeight = point.qps > 0 ? Math.max(1, (point.qps / maxValue.value) * innerHeight) : 0
    return {
      ...point,
      x: padding.left + index * slot + slot * 0.14,
      y: padding.top + innerHeight - barHeight,
      width: Math.max(1, slot * 0.68),
      height: barHeight,
    }
  })
})

const gridLines = [0.25, 0.5, 0.75].map(
  (ratio) => padding.top + (height - padding.top - padding.bottom) * ratio,
)

function timeLabel(bucket: string): string {
  const date = new Date(bucket)
  if (Number.isNaN(date.getTime())) return bucket
  return new Intl.DateTimeFormat(prefs.locale === 'en' ? 'en-US' : 'zh-CN', {
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  }).format(date)
}

const labels = computed(() => {
  const first = series.value.at(0)
  const last = series.value.at(-1)
  return { start: first ? timeLabel(first.bucket) : '', end: last ? timeLabel(last.bucket) : '' }
})
</script>

<template>
  <div class="qps-chart">
    <svg
      v-if="bars.length"
      :viewBox="'0 0 ' + width + ' ' + height"
      role="img"
      :aria-label="t('dashboard.qpsTitle')"
    >
      <line
        v-for="(line, index) in gridLines"
        :key="index"
        class="qps-grid"
        :x1="padding.left"
        :x2="width - padding.right"
        :y1="line"
        :y2="line"
      />
      <rect
        v-for="bar in bars"
        :key="bar.bucket"
        class="qps-bar"
        :x="bar.x"
        :y="bar.y"
        :width="bar.width"
        :height="bar.height"
        rx="1.5"
      >
        <title>{{ timeLabel(bar.bucket) }} · {{ bar.qps.toFixed(2) }} QPS</title>
      </rect>
    </svg>
    <div v-else class="qps-empty">{{ t('dashboard.qpsEmpty') }}</div>
    <div v-if="bars.length" class="qps-axis">
      <span>{{ labels.start }}</span>
      <span>QPS</span>
      <span>{{ labels.end }}</span>
    </div>
  </div>
</template>
