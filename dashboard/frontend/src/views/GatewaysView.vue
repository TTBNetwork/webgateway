<script setup lang="ts">
import { onMounted, ref } from 'vue'
import AppIcon from '../components/AppIcon.vue'
import EmptyState from '../components/EmptyState.vue'
import StatusBadge from '../components/StatusBadge.vue'
import { api, type Gateway } from '../api'
import { errorMessage, formatDate, formatRelative } from '../format'
import { t } from '../i18n'

const items = ref<Gateway[]>([])
const loading = ref(false)
const error = ref('')

function location(gateway: Gateway): string {
  return [gateway.city, gateway.country].filter(Boolean).join(', ') || '—'
}

async function load(): Promise<void> {
  loading.value = true
  error.value = ''
  try {
    items.value = await api.gateways()
  } catch (reason) {
    error.value = errorMessage(reason)
  } finally {
    loading.value = false
  }
}

onMounted(load)
</script>

<template>
  <div class="page">
    <header class="page-header">
      <div>
        <p class="eyebrow">{{ t('nav.gateways') }}</p>
        <h2>{{ t('gateways.title') }}</h2>
        <p class="page-subtitle">{{ t('gateways.subtitle') }}</p>
      </div>
      <button class="btn" type="button" :disabled="loading" @click="load">
        <AppIcon name="refresh" :size="16" />
        {{ t('common.refresh') }}
      </button>
    </header>

    <div v-if="error" class="error-banner">
      <AppIcon name="alert" :size="18" />
      <span>{{ error }}</span>
      <button class="btn small" type="button" @click="load">{{ t('common.retry') }}</button>
    </div>

    <div v-if="loading && items.length === 0" class="loading">{{ t('common.loading') }}…</div>

    <div v-else-if="items.length" class="gateway-grid">
      <article v-for="gateway in items" :key="gateway.id" class="card gateway-card">
        <div class="gateway-head">
          <div class="entity">
            <div class="entity-icon"><AppIcon name="gateways" :size="18" /></div>
            <div>
              <strong>{{ gateway.name || gateway.id }}</strong>
              <div class="muted small-text">{{ location(gateway) }}</div>
            </div>
          </div>
          <StatusBadge
            :status="gateway.online ? 'online' : 'offline'"
            :label="gateway.online ? t('common.online') : t('common.offline')"
          />
        </div>

        <dl class="kv">
          <div>
            <dt>{{ t('gateways.version') }}</dt>
            <dd class="mono">{{ gateway.version || '—' }}</dd>
          </div>
          <div>
            <dt>{{ t('gateways.lastSeen') }}</dt>
            <dd>{{ formatRelative(gateway.last_seen_at) }}</dd>
          </div>
          <div>
            <dt>{{ t('gateways.started') }}</dt>
            <dd>{{ formatDate(gateway.started_at) }}</dd>
          </div>
        </dl>
      </article>
    </div>

    <EmptyState
      v-else-if="!loading"
      icon="gateways"
      :title="t('gateways.empty')"
      :hint="t('gateways.emptyHint')"
    />
  </div>
</template>
