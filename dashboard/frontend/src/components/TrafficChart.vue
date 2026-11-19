<script setup lang="ts">
import { computed } from 'vue'
import type { TrafficPoint } from '../api'
import { formatNumber } from '../format'
import { prefs } from '../prefs'
import { t } from '../i18n'

const props = defineProps<{ points: TrafficPoint[] }>()

const width = 720
const height = 220
const padding = 26

const maxValue = computed(() => Math.max(1, ...props.points.map((point) => point.requests)))

const series = computed(() =>
  props.points.map((point, index) => {
    const x =
      props.points.length === 1
        ? width / 2
        : padding + (index / (props.points.length - 1)) * (width - padding * 2)
    const y = height - padding - (point.requests / maxValue.value) * (height - padding * 2)
    return { ...point, x, y }
  }),
)

const linePath = computed(() =>
  series.value
    .map(
      (point, index) => (index === 0 ? 'M' : 'L') + point.x.toFixed(1) + ' ' + point.y.toFixed(1),
    )
    .join(' '),
)

const areaPath = computed(() => {
  const first = series.value.at(0)
  const last = series.value.at(-1)
  if (!first || !last) return ''
  return (
    linePath.value +
    ' L ' +
    last.x.toFixed(1) +
    ' ' +
    String(height - padding) +
    ' L ' +
    first.x.toFixed(1) +
    ' ' +
    String(height - padding) +
    ' Z'
  )
})

function timeLabel(bucket: string): string {
  const date = new Date(bucket)
  if (Number.isNaN(date.getTime())) return bucket
  return new Intl.DateTimeFormat(prefs.locale === 'en' ? 'en-US' : 'zh-CN', {
    hour: '2-digit',
    minute: '2-digit',
  }).format(date)
}

const labels = computed(() => {
  const first = series.value.at(0)
  const last = series.value.at(-1)
  return {
    start: first ? timeLabel(first.bucket) : '',
    end: last ? timeLabel(last.bucket) : '',
  }
})
</script>

<template>
  <div class="chart">
    <svg
      v-if="series.length"
      :viewBox="'0 0 ' + width + ' ' + height"
      role="img"
      :aria-label="t('dashboard.traffic24h')"
    >
      <defs>
        <linearGradient id="traffic-fill" x1="0" x2="0" y1="0" y2="1">
          <stop offset="0%" stop-color="currentColor" stop-opacity="0.32" />
          <stop offset="100%" stop-color="currentColor" stop-opacity="0" />
        </linearGradient>
      </defs>
      <path :d="areaPath" fill="url(#traffic-fill)" class="chart-area" />
      <path
        :d="linePath"
        fill="none"
        class="chart-line"
        stroke-width="2.5"
        stroke-linejoin="round"
        stroke-linecap="round"
      />
      <circle
        v-for="point in series"
        :key="point.bucket"
        :cx="point.x"
        :cy="point.y"
        r="3"
        class="chart-dot"
      >
        <title>{{ point.bucket }} · {{ formatNumber(point.requests) }}</title>
      </circle>
    </svg>
    <div v-else class="chart-empty">{{ t('dashboard.trafficEmpty') }}</div>
    <div v-if="series.length" class="chart-axis">
      <span>{{ labels.start }}</span>
      <span>{{ t('dashboard.requests') }} · max {{ formatNumber(maxValue) }}</span>
      <span>{{ labels.end }}</span>
    </div>
  </div>
</template>
