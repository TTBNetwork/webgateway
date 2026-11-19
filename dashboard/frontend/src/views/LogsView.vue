<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue'
import AppIcon from '../components/AppIcon.vue'
import EmptyState from '../components/EmptyState.vue'
import StatusBadge from '../components/StatusBadge.vue'
import { api, type AccessLog } from '../api'
import { errorMessage, formatBytes, formatDate } from '../format'
import { t } from '../i18n'

const items = ref<AccessLog[]>([])
const loading = ref(false)
const error = ref('')
const search = ref('')
const limit = ref(50)
let timer: number | undefined

const filtered = computed(() => {
  const needle = search.value.trim().toLowerCase()
  if (!needle) return items.value
  return items.value.filter((item) => {
    const haystack = [
      item.host,
      item.path,
      item.remote_addr,
      item.method,
      String(item.status ?? ''),
    ]
      .join(' ')
      .toLowerCase()
    return haystack.includes(needle)
  })
})

async function load(): Promise<void> {
  loading.value = true
  error.value = ''
  try {
    items.value = await api.logs(limit.value)
  } catch (reason) {
    error.value = errorMessage(reason)
  } finally {
    loading.value = false
  }
}

onMounted(() => {
  void load()
  timer = window.setInterval(() => {
    if (document.visibilityState === 'visible') void load()
  }, 15_000)
})

onUnmounted(() => {
  if (timer) window.clearInterval(timer)
})
</script>

<template>
  <div class="page">
    <header class="page-header">
      <div>
        <p class="eyebrow">{{ t('nav.logs') }}</p>
        <h2>{{ t('logs.title') }}</h2>
        <p class="page-subtitle">{{ t('logs.subtitle') }}</p>
      </div>
      <button class="btn" type="button" :disabled="loading" @click="load">
        <AppIcon name="refresh" :size="16" />
        {{ t('common.refresh') }}
      </button>
    </header>

    <div class="toolbar">
      <label class="search-box">
        <AppIcon name="search" :size="16" />
        <input v-model="search" type="search" :placeholder="t('logs.search')" />
      </label>
      <label class="select-box">
        <span>{{ t('logs.limit') }}</span>
        <select v-model.number="limit" class="select" @change="load">
          <option :value="50">50</option>
          <option :value="100">100</option>
          <option :value="200">200</option>
        </select>
      </label>
      <span class="pill-note">{{ t('logs.readonly') }}</span>
    </div>

    <div v-if="error" class="error-banner">
      <AppIcon name="alert" :size="18" />
      <span>{{ error }}</span>
      <button class="btn small" type="button" @click="load">{{ t('common.retry') }}</button>
    </div>

    <div v-if="loading && items.length === 0" class="loading">{{ t('common.loading') }}…</div>

    <div v-else-if="filtered.length" class="card table-card">
      <div class="table-wrap">
        <table>
          <thead>
            <tr>
              <th>{{ t('logs.time') }}</th>
              <th>{{ t('logs.host') }}</th>
              <th>{{ t('logs.status') }}</th>
              <th>{{ t('logs.remote') }}</th>
              <th>{{ t('logs.size') }}</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="item in filtered" :key="item.id">
              <td class="muted nowrap">{{ formatDate(item.requested_at) }}</td>
              <td>
                <div class="log-target">
                  <span class="method mono">{{ item.method }}</span>
                  <div>
                    <strong>{{ item.host }}</strong>
                    <div class="muted small-text mono">{{ item.path }}</div>
                  </div>
                </div>
              </td>
              <td>
                <StatusBadge :status="item.status" />
                <div class="muted small-text">{{ item.http_version }}</div>
              </td>
              <td class="muted mono">{{ item.remote_addr }}</td>
              <td class="muted mono small-text">
                {{ formatBytes(item.request_size) }} /
                {{ item.response_size === null ? '—' : formatBytes(item.response_size) }}
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </div>

    <EmptyState
      v-else-if="!loading"
      icon="logs"
      :title="search ? t('common.empty') : t('logs.empty')"
      :hint="search ? t('logs.search') : t('logs.emptyHint')"
    />
  </div>
</template>
