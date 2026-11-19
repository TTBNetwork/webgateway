<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import AppIcon from '../components/AppIcon.vue'
import EmptyState from '../components/EmptyState.vue'
import { api, type Website } from '../api'
import { errorMessage, formatDate } from '../format'
import { t } from '../i18n'

const items = ref<Website[]>([])
const loading = ref(false)
const error = ref('')
const search = ref('')

const filtered = computed(() => {
  const needle = search.value.trim().toLowerCase()
  if (!needle) return items.value
  return items.value.filter((item) => {
    const haystack = [item.id, item.name ?? '', ...item.hosts].join(' ').toLowerCase()
    return haystack.includes(needle)
  })
})

function backendCount(value: unknown): number {
  return Array.isArray(value) ? value.length : 0
}

async function load(): Promise<void> {
  loading.value = true
  error.value = ''
  try {
    items.value = await api.websites()
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
        <p class="eyebrow">{{ t('nav.websites') }}</p>
        <h2>{{ t('websites.title') }}</h2>
        <p class="page-subtitle">{{ t('websites.subtitle') }}</p>
      </div>
      <button class="btn" type="button" :disabled="loading" @click="load">
        <AppIcon name="refresh" :size="16" />
        {{ t('common.refresh') }}
      </button>
    </header>

    <div class="toolbar">
      <label class="search-box">
        <AppIcon name="search" :size="16" />
        <input v-model="search" type="search" :placeholder="t('websites.search')" />
      </label>
      <span class="pill-note">{{ t('common.readonly') }}</span>
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
              <th>{{ t('websites.hosts') }}</th>
              <th>{{ t('websites.ports') }}</th>
              <th>{{ t('websites.backends') }}</th>
              <th>{{ t('websites.updated') }}</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="item in filtered" :key="item.id">
              <td>
                <div class="entity">
                  <div class="entity-icon"><AppIcon name="websites" :size="18" /></div>
                  <div>
                    <strong>{{ item.name || item.id }}</strong>
                    <div class="chips">
                      <span v-for="host in item.hosts" :key="host" class="chip">{{ host }}</span>
                    </div>
                  </div>
                </div>
              </td>
              <td>
                <span v-for="port in item.ports" :key="port" class="chip mono">{{ port }}</span>
              </td>
              <td>
                <span class="badge badge-info">{{ backendCount(item.backends) }}</span>
              </td>
              <td class="muted">{{ formatDate(item.updated_at) }}</td>
            </tr>
          </tbody>
        </table>
      </div>
      <p class="card-footnote">{{ t('websites.readonly') }}</p>
    </div>

    <EmptyState
      v-else-if="!loading"
      icon="websites"
      :title="search ? t('common.empty') : t('websites.empty')"
      :hint="search ? t('websites.search') : t('websites.emptyHint')"
    />
  </div>
</template>
