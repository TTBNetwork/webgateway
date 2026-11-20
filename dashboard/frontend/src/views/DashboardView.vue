<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import AppIcon from '../components/AppIcon.vue'
import EmptyState from '../components/EmptyState.vue'
import GeoMap from '../components/GeoMap.vue'
import QpsPanel from '../components/QpsPanel.vue'
import TrafficChart from '../components/TrafficChart.vue'
import { api, type GeoSummary, type Summary } from '../api'
import { errorMessage, formatBytes, formatNumber } from '../format'
import { t } from '../i18n'

interface Metric {
  label: string
  value: string
  hint?: string
  tone?: 'warning' | 'danger'
}

const summary = ref<Summary | null>(null)
const geo = ref<GeoSummary | null>(null)
const loading = ref(false)
const error = ref('')

const rates = computed(() => {
  const data = summary.value
  if (!data) return { backend: 0, e4xx: 0, e5xx: 0 }
  const total = data.requests_today
  const percent = (count: number) => (total > 0 ? (count / total) * 100 : 0)
  return {
    backend: percent(data.backend_errors_today),
    e4xx: percent(data.e4xx_today),
    e5xx: percent(data.e5xx_today),
  }
})

// v1's statistics page groups two numbers per panel and uses total requests as
// the denominator for all error rates.
const metricGroups = computed<Metric[][]>(() => {
  const data = summary.value
  if (!data) return []
  return [
    [
      {
        label: t('dashboard.requestCount'),
        value: formatNumber(data.requests_today),
        hint: t('dashboard.today'),
      },
      {
        label: t('dashboard.uniqueIPs'),
        value: formatNumber(data.unique_ips_today),
        hint: t('dashboard.ipHint'),
      },
    ],
    [
      {
        label: t('dashboard.backendErrors'),
        value: formatNumber(data.backend_errors_today),
        hint: t('dashboard.backendErrorHint'),
        tone: data.backend_errors_today > 0 ? 'danger' : undefined,
      },
      {
        label: t('dashboard.backendErrorRate'),
        value: rates.value.backend.toFixed(2) + '%',
        hint: t('dashboard.rateOfRequests'),
        tone: rates.value.backend > 0 ? 'danger' : undefined,
      },
    ],
    [
      {
        label: t('dashboard.e4xxCount'),
        value: formatNumber(data.e4xx_today),
        hint: t('dashboard.rateOfRequests'),
        tone: data.e4xx_today > 0 ? 'warning' : undefined,
      },
      {
        label: t('dashboard.e4xxRate'),
        value: rates.value.e4xx.toFixed(2) + '%',
        hint: t('dashboard.rateOfRequests'),
        tone: rates.value.e4xx > 0 ? 'warning' : undefined,
      },
    ],
    [
      {
        label: t('dashboard.e5xxCount'),
        value: formatNumber(data.e5xx_today),
        hint: t('dashboard.rateOfRequests'),
        tone: data.e5xx_today > 0 ? 'danger' : undefined,
      },
      {
        label: t('dashboard.e5xxRate'),
        value: rates.value.e5xx.toFixed(2) + '%',
        hint: t('dashboard.rateOfRequests'),
        tone: rates.value.e5xx > 0 ? 'danger' : undefined,
      },
    ],
  ]
})

const byteCards = computed<Metric[]>(() => {
  const data = summary.value
  if (!data) return []
  return [
    { label: t('dashboard.requestSize'), value: formatBytes(data.requests_size_today) },
    { label: t('dashboard.responseSize'), value: formatBytes(data.response_size_today) },
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
      <section class="v1-metrics">
        <div class="v1-metric-grid">
          <article
            v-for="(group, groupIndex) in metricGroups"
            :key="groupIndex"
            class="v1-metric-card"
          >
            <div v-for="metric in group" :key="metric.label" class="v1-metric-cell">
              <span class="stat-label">{{ metric.label }}</span>
              <strong class="v1-metric-value" :class="metric.tone ? 'text-' + metric.tone : ''">
                {{ metric.value }}
              </strong>
              <span v-if="metric.hint" class="stat-hint">{{ metric.hint }}</span>
            </div>
          </article>
        </div>

        <div class="v1-byte-stack">
          <article v-for="metric in byteCards" :key="metric.label" class="v1-metric-card single">
            <div class="v1-metric-cell">
              <span class="stat-label">{{ metric.label }}</span>
              <strong class="v1-metric-value">{{ metric.value }}</strong>
            </div>
          </article>
        </div>
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
