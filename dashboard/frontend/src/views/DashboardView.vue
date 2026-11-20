<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import AppIcon from '../components/AppIcon.vue'
import EmptyState from '../components/EmptyState.vue'
import GeoMap from '../components/GeoMap.vue'
import QpsPanel from '../components/QpsPanel.vue'
import StatCard from '../components/StatCard.vue'
import TrafficChart from '../components/TrafficChart.vue'
import { api, type GeoSummary, type Summary } from '../api'
import { errorMessage, formatBytes, formatNumber } from '../format'
import { t } from '../i18n'

interface Card {
  label: string
  value: string
  hint: string
  icon: string
  tone: 'default' | 'accent' | 'success' | 'warning' | 'danger'
}

const summary = ref<Summary | null>(null)
const geo = ref<GeoSummary | null>(null)
const loading = ref(false)
const error = ref('')

const cards = computed<Card[]>(() => {
  const data = summary.value
  if (!data) return []
  const rate4 = data.responses_today > 0 ? (data.e4xx_today / data.responses_today) * 100 : 0
  const rate5 = data.responses_today > 0 ? (data.e5xx_today / data.responses_today) * 100 : 0

  return [
    {
      label: t('dashboard.requestCount'),
      value: formatNumber(data.requests_today),
      hint: t('dashboard.today'),
      icon: 'logs',
      tone: 'default' as const,
    },
    {
      label: t('dashboard.pv'),
      value: formatNumber(data.responses_today),
      hint: t('dashboard.pvHint'),
      icon: 'websites',
      tone: 'default' as const,
    },
    {
      label: t('dashboard.uv'),
      value: formatNumber(data.uv_today),
      hint: t('dashboard.uvHint'),
      icon: 'users',
      tone: 'default' as const,
    },
    {
      label: t('dashboard.uniqueIPs'),
      value: formatNumber(data.unique_ips_today),
      hint: t('dashboard.ipHint'),
      icon: 'gateways',
      tone: 'default' as const,
    },
    {
      label: t('dashboard.e4xxCount'),
      value: formatNumber(data.e4xx_today),
      hint: t('dashboard.errorRateHint', { rate: rate4.toFixed(2) + '%' }),
      icon: 'alert',
      tone: data.e4xx_today > 0 ? ('warning' as const) : ('success' as const),
    },
    {
      label: t('dashboard.e5xxCount'),
      value: formatNumber(data.e5xx_today),
      hint: t('dashboard.errorRateHint', { rate: rate5.toFixed(2) + '%' }),
      icon: 'alert',
      tone: data.e5xx_today > 0 ? ('danger' as const) : ('success' as const),
    },
  ]
})

async function load(): Promise<void> {
  loading.value = true
  error.value = ''
  try {
    summary.value = await api.summary()
  } catch (reason) {
    error.value = errorMessage(reason)
  } finally {
    loading.value = false
  }
}

// Country aggregation scans raw IPs; it changes slowly and does not need the
// 15s summary cadence. Keep the previous map on transient failures.
async function loadGeo(): Promise<void> {
  try {
    geo.value = await api.geo()
  } catch {
    // The map is optional; summary errors already surface the dashboard error.
  }
}

// No page-wide polling: the QPS card owns its 5s clock, while summary/map
// load once on mount.
onMounted(() => {
  void load()
  void loadGeo()
})
</script>

<template>
  <div class="page">
    <header class="page-header">
      <div>
        <p class="eyebrow">{{ t('nav.dashboard') }}</p>
        <h2>{{ t('dashboard.title') }}</h2>
        <p class="page-subtitle">{{ t('dashboard.subtitle') }}</p>
      </div>
      <span class="pill-note">{{ t('dashboard.autoRefresh') }}</span>
    </header>

    <div v-if="error" class="error-banner">
      <AppIcon name="alert" :size="18" />
      <span>{{ t('common.error') }}: {{ error }}</span>
    </div>

    <div v-if="!summary && loading" class="loading">{{ t('common.loading') }}…</div>

    <template v-if="summary">
      <section class="stat-grid">
        <StatCard
          v-for="card in cards"
          :key="card.label"
          :label="card.label"
          :value="card.value"
          :hint="card.hint"
          :icon="card.icon"
          :tone="card.tone"
        />
      </section>

      <section class="dashboard-map-grid">
        <GeoMap
          :countries="geo?.countries ?? {}"
          :nodes="geo?.nodes ?? []"
          :configured="geo?.configured ?? false"
          :unknown="geo?.unknown ?? 0"
        />

        <div class="dashboard-stack">
          <QpsPanel />

          <div class="card">
            <div class="card-header">
              <div>
                <h3>{{ t('dashboard.breakdown') }}</h3>
                <p>{{ t('dashboard.requests') }} / {{ t('dashboard.responses') }}</p>
              </div>
            </div>
            <dl class="metric-list">
              <div>
                <dt>{{ t('dashboard.requests') }}</dt>
                <dd>{{ formatNumber(summary.requests_today) }}</dd>
              </div>
              <div>
                <dt>{{ t('dashboard.responses') }}</dt>
                <dd>{{ formatNumber(summary.responses_today) }}</dd>
              </div>
              <div>
                <dt>{{ t('dashboard.e4xx') }}</dt>
                <dd class="text-warning">{{ formatNumber(summary.e4xx_today) }}</dd>
              </div>
              <div>
                <dt>{{ t('dashboard.e5xx') }}</dt>
                <dd class="text-danger">{{ formatNumber(summary.e5xx_today) }}</dd>
              </div>
              <div>
                <dt>{{ t('dashboard.requestSize') }}</dt>
                <dd>{{ formatBytes(summary.requests_size_today) }}</dd>
              </div>
              <div>
                <dt>{{ t('dashboard.responseSize') }}</dt>
                <dd>{{ formatBytes(summary.response_size_today) }}</dd>
              </div>
            </dl>
          </div>
        </div>
      </section>

      <section class="card chart-card traffic-card">
        <div class="card-header">
          <div>
            <h3>{{ t('dashboard.traffic24h') }}</h3>
            <p>{{ t('dashboard.requests') }} · hourly</p>
          </div>
          <span class="badge badge-info">{{ t('dashboard.requestsHint') }}</span>
        </div>
        <TrafficChart :points="summary.traffic" />
      </section>
    </template>

    <EmptyState
      v-else-if="!loading && !error"
      icon="dashboard"
      :title="t('common.empty')"
      :hint="t('dashboard.subtitle')"
    />
  </div>
</template>
