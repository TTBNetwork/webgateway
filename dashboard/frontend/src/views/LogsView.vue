<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import AppIcon from '../components/AppIcon.vue'
import EmptyState from '../components/EmptyState.vue'
import AppModal from '../components/AppModal.vue'
import SelectField from '../components/SelectField.vue'
import StatusBadge from '../components/StatusBadge.vue'
import { api, type AccessLog } from '../api'
import { usePolling } from '../composables/usePolling'
import { errorMessage, formatBytes, formatDate } from '../format'
import { t } from '../i18n'

const items = ref<AccessLog[]>([])
const loading = ref(false)
const error = ref('')
const search = ref('')
const limit = ref(50)
const selected = ref<AccessLog | null>(null)

const limitOptions = [
  { value: 50, label: '50' },
  { value: 100, label: '100' },
  { value: 200, label: '200' },
]

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
      item.website_id ?? '',
    ]
      .join(' ')
      .toLowerCase()
    return haystack.includes(needle)
  })
})

function headers(value: Record<string, unknown> | null): Array<[string, string]> {
  if (!value) return []
  return Object.entries(value).map(([name, raw]) => [
    name,
    typeof raw === 'string' ? raw : JSON.stringify(raw),
  ])
}

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

usePolling(load, 15_000)

watch(limit, () => {
  void load()
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
      <span class="pill-note">{{ t('logs.autoRefresh') }}</span>
    </header>

    <div class="toolbar">
      <label class="search-box">
        <AppIcon name="search" :size="16" />
        <input v-model="search" type="search" :placeholder="t('logs.search')" />
      </label>
      <label class="select-box">
        <span>{{ t('logs.limit') }}</span>
        <SelectField v-model="limit" :options="limitOptions" />
      </label>
      <span class="pill-note">{{ t('logs.readonly') }}</span>
    </div>

    <div v-if="error" class="error-banner">
      <AppIcon name="alert" :size="18" />
      <span>{{ error }}</span>
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
            <tr
              v-for="item in filtered"
              :key="item.id"
              class="clickable-row"
              tabindex="0"
              @click="selected = item"
              @keydown.enter="selected = item"
            >
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
      <p class="card-footnote">{{ t('logs.clickHint') }}</p>
    </div>

    <EmptyState
      v-else-if="!loading"
      icon="logs"
      :title="search ? t('common.empty') : t('logs.empty')"
      :hint="search ? t('logs.search') : t('logs.emptyHint')"
    />

    <AppModal v-if="selected" :title="t('logs.detail')" wide @close="selected = null">
      <div class="detail-head">
        <span class="method mono">{{ selected.method }}</span>
        <StatusBadge :status="selected.status" />
        <span class="mono">{{ selected.host }}{{ selected.path }}</span>
      </div>
      <dl class="detail-grid">
        <div>
          <dt>{{ t('logs.time') }}</dt>
          <dd>{{ formatDate(selected.requested_at) }}</dd>
        </div>
        <div>
          <dt>{{ t('logs.remote') }}</dt>
          <dd class="mono">{{ selected.remote_addr }}</dd>
        </div>
        <div>
          <dt>{{ t('logs.version') }}</dt>
          <dd class="mono">{{ selected.http_version }}</dd>
        </div>
        <div>
          <dt>{{ t('logs.website') }}</dt>
          <dd class="mono">{{ selected.website_id || '—' }}</dd>
        </div>
        <div>
          <dt>{{ t('logs.size') }}</dt>
          <dd class="mono">
            {{ formatBytes(selected.request_size) }} /
            {{ selected.response_size === null ? '—' : formatBytes(selected.response_size) }}
          </dd>
        </div>
        <div>
          <dt>ID</dt>
          <dd class="mono small-text">{{ selected.id }}</dd>
        </div>
      </dl>

      <div class="detail-columns">
        <section>
          <h4>{{ t('logs.requestHeaders') }}</h4>
          <div v-if="headers(selected.request_headers).length" class="header-list">
            <div
              v-for="[name, value] in headers(selected.request_headers)"
              :key="'req-' + name"
              class="header-row"
            >
              <span class="mono">{{ name }}</span>
              <span class="mono">{{ value }}</span>
            </div>
          </div>
          <p v-else class="muted small-text">{{ t('logs.noHeaders') }}</p>
        </section>
        <section>
          <h4>{{ t('logs.responseHeaders') }}</h4>
          <div v-if="headers(selected.response_headers).length" class="header-list">
            <div
              v-for="[name, value] in headers(selected.response_headers)"
              :key="'res-' + name"
              class="header-row"
            >
              <span class="mono">{{ name }}</span>
              <span class="mono">{{ value }}</span>
            </div>
          </div>
          <p v-else class="muted small-text">{{ t('logs.noHeaders') }}</p>
        </section>
      </div>
    </AppModal>
  </div>
</template>
