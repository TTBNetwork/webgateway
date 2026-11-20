<script setup lang="ts">
import { computed } from 'vue'
import EChart from './EChart.vue'
import type { QpsPoint } from '../api'
import { chartTheme } from '../charts'
import { prefs } from '../prefs'
import { t } from '../i18n'

const props = defineProps<{ points: QpsPoint[] }>()

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
  const first = props.points.at(0)
  const last = props.points.at(-1)
  return {
    start: first ? timeLabel(first.bucket) : '',
    end: last ? timeLabel(last.bucket) : '',
  }
})

const option = computed<Record<string, unknown>>(() => {
  const theme = chartTheme()
  return {
    animationDuration: 280,
    animationEasing: 'cubicOut',
    grid: { left: 2, right: 2, top: 8, bottom: 2, containLabel: false },
    tooltip: {
      trigger: 'axis',
      axisPointer: { type: 'shadow', shadowStyle: { color: theme.border } },
      backgroundColor: theme.panel,
      borderColor: theme.border,
      textStyle: { color: theme.text, fontSize: 11 },
      formatter: (params: { name: string; value: number }[]) => {
        const first = params[0]
        return first ? first.name + '<br/>' + Number(first.value).toFixed(2) + ' QPS' : ''
      },
    },
    xAxis: {
      type: 'category',
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
        name: 'QPS',
        type: 'bar',
        barCategoryGap: '38%',
        data: props.points.map((point) => point.qps),
        itemStyle: {
          color: theme.accent,
          borderRadius: [2, 2, 0, 0],
        },
      },
    ],
  }
})
</script>

<template>
  <div class="qps-chart">
    <EChart v-if="points.length" :option="option" class="qps-echart" />
    <div v-else class="qps-empty">{{ t('dashboard.qpsEmpty') }}</div>
    <div v-if="points.length" class="qps-axis">
      <span>{{ labels.start }}</span>
      <span>QPS</span>
      <span>{{ labels.end }}</span>
    </div>
  </div>
</template>
