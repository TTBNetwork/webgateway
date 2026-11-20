<script setup lang="ts">
import { computed, ref } from 'vue'
import AppIcon from '../components/AppIcon.vue'
import EmptyState from '../components/EmptyState.vue'
import StatCard from '../components/StatCard.vue'
import TrafficChart from '../components/TrafficChart.vue'
import { api, type Summary } from '../api'
import { usePolling } from '../composables/usePolling'
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
const loading = ref(false)
const error = ref('')

const cards = computed<Card[]>(() => {
  const data = summary.value
  if (!data) return []
  const errors = data.e4xx_today + data.e5xx_today
  const rate = data.responses_today > 0 ? (errors / data.responses_today) * 100 : 0
  const totalBytes = Number(data.requests_size_today) + Number(data.response_size_today)

  return [
    {
      label: t('dashboard.websites'),
      value: formatNumber(data.websites),
      hint: t('dashboard.websitesHint'),
      icon: 'websites',
      tone: 'accent' as const,
    },
    {
      label: t('dashboard.certificates'),
      value: formatNumber(data.certificates),
      hint: t('certs.subtitle'),
      icon: 'certificates',
      tone: 'default' as const,
    },
    {
      label: t('dashboard.gateways'),
      value: formatNumber(data.gateways_online) + ' / ' + formatNumber(data.gateways_total),
      hint: t('dashboard.gatewaysHint'),
      icon: 'gateways',
      tone: data.gateways_online > 0 ? ('success' as const) : ('warning' as const),
    },
    {
      label: t('dashboard.requestsToday'),
      value: formatNumber(data.requests_today),
      hint: t('dashboard.requestsHint'),
      icon: 'logs',
      tone: 'default' as const,
    },
    {
      label: t('dashboard.errorsToday'),
      value: formatNumber(errors),
      hint: t('dashboard.errorsHint'),
      icon: 'alert',
      tone: errors > 0 ? ('warning' as const) : ('success' as const),
    },
    {
      label: t('dashboard.errorRate'),
      value: rate.toFixed(1) + '%',
      hint: formatBytes(totalBytes) + ' ' + t('dashboard.traffic24h'),
      icon: 'dashboard',
      tone:
        rate >= 5 ? ('danger' as const) : rate > 0 ? ('warning' as const) : ('success' as const),
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

usePolling(load)
</script>

<template>
  <div class="page">
    <header class="page-header">
      <div>
        <p class="eyebrow">{{ t('nav.dashboard') }}</p>
        <h2>{{ t('dashboard.title') }}</h2>
        <p class="page-subtitle">{{ t('dashboard.subtitle') }}</p>
      </div>
      <span class="pill-note">{{ t('websites.autoRefresh') }}</span>
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

      <section class="dashboard-grid">
        <div class="card chart-card">
          <div class="card-header">
            <div>
              <h3>{{ t('dashboard.traffic24h') }}</h3>
              <p>{{ t('dashboard.requests') }} · hourly</p>
            </div>
            <span class="badge badge-info">{{ t('dashboard.requestsHint') }}</span>
          </div>
          <TrafficChart :points="summary.traffic" />
        </div>

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
