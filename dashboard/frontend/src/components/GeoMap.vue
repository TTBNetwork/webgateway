<script setup lang="ts">
import { computed } from 'vue'
import worldData from '../assets/world-map.json'
import type { GeoNode } from '../api'
import { formatNumber } from '../format'
import { t } from '../i18n'
import { prefs } from '../prefs'

interface WorldCountry {
  iso: string | null
  name: string
  nameZh: string
  path: string
  point: [number, number] | null
}

interface WorldMap {
  width: number
  height: number
  graticule: string
  countries: WorldCountry[]
}

const world = worldData as unknown as WorldMap

function countryName(country: WorldCountry): string {
  return prefs.locale === 'zh' && country.nameZh ? country.nameZh : country.name
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
    const feature = world.countries.find((country) => country.iso === code && country.point)
    if (feature) result.push({ code, count, feature })
  }
  return result.sort((a, b) => b.count - a.count)
})

const topSources = computed(() => normalized.value.slice(0, 6))
const sourceCodes = computed(() => new Set(normalized.value.map((item) => item.code)))
const countryCount = computed(() => normalized.value.length)
const totalSources = computed(() => normalized.value.reduce((sum, item) => sum + item.count, 0))
const maxSource = computed(() => Math.max(1, ...normalized.value.map((item) => item.count)))

const nodeMarkers = computed(() => {
  const seen = new Map<string, number>()
  return props.nodes.flatMap((node) => {
    const code = (node.country ?? '').toUpperCase()
    const feature = world.countries.find((country) => country.iso === code && country.point)
    if (!feature || !feature.point) return []
    const key = code
    const index = seen.get(key) ?? 0
    seen.set(key, index + 1)
    return [
      {
        id: node.id,
        name: node.name ?? node.city ?? code,
        online: node.online,
        x: feature.point[0] + index * 8,
        y: feature.point[1] - index * 5,
      },
    ]
  })
})

const sourceMarkers = computed(() =>
  normalized.value.flatMap((item) =>
    item.feature.point
      ? [
          {
            code: item.code,
            count: item.count,
            name: countryName(item.feature),
            x: item.feature.point[0],
            y: item.feature.point[1],
          },
        ]
      : [],
  ),
)

const onlineNodes = computed(() => props.nodes.filter((node) => node.online).length)
</script>

<template>
  <section class="card geo-card">
    <div class="card-header geo-header">
      <div>
        <h3>{{ t('dashboard.geoTitle') }}</h3>
        <p>{{ t('dashboard.geoSubtitle') }}</p>
      </div>
      <span class="geo-scope">{{ t('dashboard.geoGlobal') }}</span>
    </div>

    <div class="geo-canvas">
      <svg
        :viewBox="'0 0 ' + world.width + ' ' + world.height"
        role="img"
        :aria-label="t('dashboard.geoTitle')"
      >
        <path class="geo-graticule" :d="world.graticule" />
        <path
          v-for="country in world.countries"
          :key="(country.iso ?? '') + country.name"
          class="geo-country"
          :class="{ 'has-source': country.iso && sourceCodes.has(country.iso) }"
          :d="country.path"
        />

        <g v-for="marker in sourceMarkers" :key="marker.code" class="geo-source-marker">
          <circle class="geo-pulse" :cx="marker.x" :cy="marker.y" r="13" />
          <circle class="geo-source-dot" :cx="marker.x" :cy="marker.y" r="4.5" />
          <title>{{ marker.name }} · {{ formatNumber(marker.count) }}</title>
        </g>

        <g v-for="node in nodeMarkers" :key="node.id" class="geo-node-marker">
          <circle class="geo-node-halo" :cx="node.x" :cy="node.y" r="7" />
          <circle class="geo-node-dot" :cx="node.x" :cy="node.y" r="3.2" />
          <title>
            {{ node.name }} · {{ node.online ? t('common.online') : t('common.offline') }}
          </title>
        </g>
      </svg>

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
      <span class="geo-attribution">Natural Earth</span>
    </footer>
  </section>
</template>
