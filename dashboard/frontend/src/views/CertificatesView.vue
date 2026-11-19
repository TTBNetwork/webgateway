<script setup lang="ts">
import { onMounted, ref } from 'vue'
import AppIcon from '../components/AppIcon.vue'
import EmptyState from '../components/EmptyState.vue'
import StatusBadge from '../components/StatusBadge.vue'
import { api, type Certificate } from '../api'
import { daysUntil, errorMessage, formatDate } from '../format'
import { t } from '../i18n'

type CertStatus = 'valid' | 'expiring' | 'expired' | 'unknown'

const items = ref<Certificate[]>([])
const loading = ref(false)
const error = ref('')

function certificateStatus(expires: string | null): CertStatus {
  const days = daysUntil(expires)
  if (days === null) return 'unknown'
  if (days < 0) return 'expired'
  if (days <= 30) return 'expiring'
  return 'valid'
}

function statusLabel(status: CertStatus): string {
  if (status === 'valid') return t('certs.valid')
  if (status === 'expiring') return t('certs.expiring')
  if (status === 'expired') return t('certs.expired')
  return t('certs.unknown')
}

function expiryHint(expires: string | null): string {
  const days = daysUntil(expires)
  if (days === null) return '—'
  return t('certs.daysLeft', { days })
}

async function load(): Promise<void> {
  loading.value = true
  error.value = ''
  try {
    items.value = await api.certificates()
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
        <p class="eyebrow">{{ t('nav.certificates') }}</p>
        <h2>{{ t('certs.title') }}</h2>
        <p class="page-subtitle">{{ t('certs.subtitle') }}</p>
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

    <div v-else-if="items.length" class="card table-card">
      <div class="table-wrap">
        <table>
          <thead>
            <tr>
              <th>{{ t('certs.hostnames') }}</th>
              <th>{{ t('certs.status') }}</th>
              <th>{{ t('certs.expires') }}</th>
              <th>{{ t('certs.created') }}</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="item in items" :key="item.id">
              <td>
                <div class="entity">
                  <div class="entity-icon"><AppIcon name="certificates" :size="18" /></div>
                  <div>
                    <strong>{{ item.name || item.id }}</strong>
                    <div class="chips">
                      <span v-for="host in item.hostnames" :key="host" class="chip">{{
                        host
                      }}</span>
                    </div>
                  </div>
                </div>
              </td>
              <td>
                <StatusBadge
                  :status="certificateStatus(item.expires_at)"
                  :label="statusLabel(certificateStatus(item.expires_at))"
                />
                <div class="muted small-text">{{ expiryHint(item.expires_at) }}</div>
              </td>
              <td class="muted">{{ formatDate(item.expires_at) }}</td>
              <td class="muted">{{ formatDate(item.created_at) }}</td>
            </tr>
          </tbody>
        </table>
      </div>
      <p class="card-footnote">{{ t('certs.readonly') }}</p>
    </div>

    <EmptyState
      v-else-if="!loading"
      icon="certificates"
      :title="t('certs.empty')"
      :hint="t('certs.emptyHint')"
    />
  </div>
</template>
