<script setup lang="ts">
import { computed } from 'vue'
import EChart from './EChart.vue'
import worldData from '../assets/world-map.json'
import type { GeoNode } from '../api'
import { chartTheme } from '../charts'
import { formatNumber } from '../format'
import { t } from '../i18n'
import { prefs } from '../prefs'

interface WorldCountry {
  iso: string | null
  name: string
  nameZh: string
  coord: [number, number] | null
}

interface WorldMap {
  countries: WorldCountry[]
}

const world = worldData as unknown as WorldMap

// SafeLine's ECharts world map uses Natural Earth short names. Most match our
// ISO lookup, these ten are the exceptions.
const ECHARTS_ALIASES: Record<string, string> = {
  US: 'United States',
  SZ: 'Swaziland',
  LA: 'Lao PDR',
  KP: 'Dem. Rep. Korea',
  KR: 'Korea',
  CZ: 'Czech Rep.',
  MK: 'Macedonia',
  XK: 'Kosovo',
}

function countryName(country: WorldCountry): string {
  return prefs.locale === 'zh' && country.nameZh ? country.nameZh : country.name
}

function mapName(country: WorldCountry): string {
  const alias = country.iso ? ECHARTS_ALIASES[country.iso] : undefined
  return alias ?? country.name
}

const props = withDefaults(
  defineProps<{
    countries: Record<string, number>
    nodes: GeoNode[]
    configured?: boolean
    unknown?: number
  }>(),
  { configured: false, unknown: 0 },
)

const normalized = computed(() => {
  const result: { code: string; count: number; feature: WorldCountry }[] = []
  for (const [rawCode, count] of Object.entries(props.countries)) {
    const code = rawCode.toUpperCase()
    if (!(count > 0)) continue
    const feature = world.countries.find((country) => country.iso === code && country.coord)
    if (feature) result.push({ code, count, feature })
  }
  return result.sort((a, b) => b.count - a.count)
})

const topSources = computed(() => normalized.value.slice(0, 6))
const countryCount = computed(() => normalized.value.length)
const totalSources = computed(() => normalized.value.reduce((sum, item) => sum + item.count, 0))
const maxSource = computed(() => Math.max(1, ...normalized.value.map((item) => item.count)))

const mapData = computed(() =>
  normalized.value.map((item) => ({
    name: mapName(item.feature),
    value: item.count,
  })),
)

const sourceMarkers = computed(() =>
  normalized.value.flatMap((item) =>
    item.feature.coord
      ? [
          {
            name: countryName(item.feature),
            value: [item.feature.coord[0], item.feature.coord[1], item.count],
          },
        ]
      : [],
  ),
)

const nodeMarkers = computed(() => {
  const seen = new Map<string, number>()
  return props.nodes.flatMap((node) => {
    const code = (node.country ?? '').toUpperCase()
    const feature = world.countries.find((country) => country.iso === code && country.coord)
    if (!feature?.coord) return []
    const index = seen.get(code) ?? 0
    seen.set(code, index + 1)
    return [
      {
        name: node.name ?? node.city ?? code,
        value: [feature.coord[0] + index * 0.6, feature.coord[1] - index * 0.4, 1],
        online: node.online,
      },
    ]
  })
})

const onlineNodes = computed(() => props.nodes.filter((node) => node.online).length)

const option = computed<Record<string, unknown>>(() => {
  const theme = chartTheme()
  return {
    backgroundColor: 'transparent',
    animationDuration: 420,
    tooltip: {
      trigger: 'item',
      backgroundColor: theme.panel,
      borderColor: theme.border,
      textStyle: { color: theme.text, fontSize: 11 },
      formatter: (params: { name: string; value: number | number[]; seriesType?: string }) => {
        if (params.seriesType === 'effectScatter') {
          const count = Array.isArray(params.value) ? params.value[2] : 0
          return params.name + '<br/>' + formatNumber(count) + ' ' + t('dashboard.requests')
        }
        return (
          params.name +
          '<br/>' +
          formatNumber(Number(params.value) || 0) +
          ' ' +
          t('dashboard.requests')
        )
      },
    },
    visualMap: {
      show: false,
      min: 0,
      max: maxSource.value,
      seriesIndex: 0,
      inRange: { color: [theme.mapLand, theme.accent] },
    },
    geo: {
      map: 'world',
      roam: false,
      zoom: 1.05,
      left: 0,
      right: 0,
      top: 4,
      bottom: 0,
      itemStyle: {
        areaColor: theme.mapLand,
        borderColor: theme.mapStroke,
        borderWidth: 0.65,
      },
      emphasis: { disabled: true },
      select: { disabled: true },
    },
    series: [
      {
        type: 'map',
        map: 'world',
        geoIndex: 0,
        data: mapData.value,
        selectedMode: false,
        emphasis: { disabled: true },
        itemStyle: { borderColor: theme.mapStroke, borderWidth: 0.65 },
      },
      {
        type: 'effectScatter',
        coordinateSystem: 'geo',
        geoIndex: 0,
        data: sourceMarkers.value,
        symbolSize: (value: number[]) => 4 + Math.sqrt((value[2] ?? 1) / maxSource.value) * 10,
        rippleEffect: { brushType: 'stroke', scale: 2.6, period: 3.4 },
        itemStyle: { color: theme.accent, shadowBlur: 8, shadowColor: theme.accent },
        zlevel: 2,
      },
      {
        type: 'effectScatter',
        coordinateSystem: 'geo',
        geoIndex: 0,
        data: nodeMarkers.value,
        symbolSize: 7,
        rippleEffect: { brushType: 'stroke', scale: 2.2, period: 3 },
        itemStyle: { color: theme.teal, shadowBlur: 8, shadowColor: theme.teal },
        zlevel: 2,
      },
    ],
  }
})
</script>

<template>
  <section class="card geo-card">
    <div class="card-header geo-header">
      <div>
        <h3>{{ t('dashboard.geoTitle') }}</h3>
        <p>{{ t('dashboard.geoSubtitle') }}</p>
      </div>
      <div class="geo-modes">
        <span class="geo-mode is-active">2D</span>
        <span class="geo-scope">{{ t('dashboard.geoGlobal') }}</span>
      </div>
    </div>

    <div class="geo-canvas">
      <EChart :option="option" map class="geo-echart" />

      <div v-if="topSources.length" class="geo-ranking">
        <p class="geo-ranking-title">{{ t('dashboard.geoSources') }}</p>
        <div v-for="item in topSources" :key="item.code" class="geo-ranking-row">
          <span class="geo-ranking-name">{{ countryName(item.feature) }}</span>
          <strong>{{ formatNumber(item.count) }}</strong>
          <i :style="{ width: (item.count / maxSource) * 100 + '%' }"></i>
        </div>
      </div>

      <div v-if="!configured" class="geo-notice">
        <strong>{{ t('dashboard.geoUnconfigured') }}</strong>
        <span>{{ t('dashboard.geoUnconfiguredHint') }}</span>
      </div>
      <div v-else-if="!sourceMarkers.length" class="geo-notice">
        <strong>{{ t('dashboard.geoEmpty') }}</strong>
        <span>{{ t('dashboard.geoEmptyHint') }}</span>
      </div>
    </div>

    <footer class="geo-footer">
      <span>
        {{ t('dashboard.geoCountries') }} {{ formatNumber(countryCount) }}
        <template v-if="totalSources > 0">
          · {{ formatNumber(totalSources) }} {{ t('dashboard.requests') }}
        </template>
        <template v-if="unknown > 0">
          · {{ t('dashboard.geoUnknown') }} {{ formatNumber(unknown) }}</template
        >
      </span>
      <span>{{ t('dashboard.geoNodes') }} {{ onlineNodes }} / {{ nodes.length }}</span>
      <span class="geo-attribution">Natural Earth · Apache ECharts</span>
    </footer>
  </section>
</template>
