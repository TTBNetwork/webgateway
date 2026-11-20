<script setup lang="ts">
import { computed } from 'vue'
import EChart from './EChart.vue'
import type { TrafficPoint } from '../api'
import { chartTheme } from '../charts'
import { formatNumber } from '../format'
import { t } from '../i18n'
import { prefs } from '../prefs'

const props = defineProps<{ points: TrafficPoint[] }>()

function timeLabel(bucket: string): string {
  const date = new Date(bucket)
  if (Number.isNaN(date.getTime())) return bucket
  return new Intl.DateTimeFormat(prefs.locale === 'en' ? 'en-US' : 'zh-CN', {
    hour: '2-digit',
    minute: '2-digit',
  }).format(date)
}

const labels = computed(() => {
  const first = props.points.at(0)
  const last = props.points.at(-1)
  return {
    start: first ? timeLabel(first.bucket) : '',
    end: last ? timeLabel(last.bucket) : '',
  }
})

const maxValue = computed(() => Math.max(1, ...props.points.map((point) => point.requests)))

const option = computed<Record<string, unknown>>(() => {
  const theme = chartTheme()
  return {
    animationDuration: 320,
    animationEasing: 'cubicOut',
    grid: { left: 4, right: 4, top: 12, bottom: 6, containLabel: false },
    tooltip: {
      trigger: 'axis',
      backgroundColor: theme.panel,
      borderColor: theme.border,
      textStyle: { color: theme.text, fontSize: 11 },
      formatter: (params: { name: string; value: number }[]) => {
        const first = params[0]
        return first
          ? first.name + '<br/>' + formatNumber(first.value) + ' ' + t('dashboard.requests')
          : ''
      },
    },
    xAxis: {
      type: 'category',
      boundaryGap: false,
      data: props.points.map((point) => timeLabel(point.bucket)),
      axisLine: { show: false },
      axisTick: { show: false },
      axisLabel: { show: false },
    },
    yAxis: {
      type: 'value',
      show: false,
      splitLine: {
        lineStyle: { color: theme.border, type: 'dashed' },
      },
    },
    series: [
      {
        name: t('dashboard.requests'),
        type: 'line',
        smooth: 0.28,
        showSymbol: false,
        data: props.points.map((point) => point.requests),
        lineStyle: { color: theme.accent, width: 2.2 },
        areaStyle: {
          color: {
            type: 'linear',
            x: 0,
            y: 0,
            x2: 0,
            y2: 1,
            colorStops: [
              { offset: 0, color: theme.accent + '55' },
              { offset: 1, color: theme.accent + '00' },
            ],
          },
        },
      },
    ],
  }
})
</script>

<template>
  <div class="chart">
    <EChart v-if="points.length" :option="option" class="chart-echart" />
    <div v-else class="chart-empty">{{ t('dashboard.trafficEmpty') }}</div>
    <div v-if="points.length" class="chart-axis">
      <span>{{ labels.start }}</span>
      <span>{{ t('dashboard.requests') }} · max {{ formatNumber(maxValue) }}</span>
      <span>{{ labels.end }}</span>
    </div>
  </div>
</template>
