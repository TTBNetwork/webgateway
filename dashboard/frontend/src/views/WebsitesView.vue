<script setup lang="ts">
import { computed, reactive, ref } from 'vue'
import AppIcon from '../components/AppIcon.vue'
import EmptyState from '../components/EmptyState.vue'
import AppModal from '../components/AppModal.vue'
import { api, type Website, type WebsiteInput } from '../api'
import { usePolling } from '../composables/usePolling'
import { errorMessage, formatDate } from '../format'
import { t } from '../i18n'
import { session } from '../session'
import { notifySuccess } from '../toast'

interface BackendDraft {
  url: string
  weight: number
  priority: number
  extra: Record<string, unknown>
}

const items = ref<Website[]>([])
const loading = ref(false)
const error = ref('')
const search = ref('')

const showForm = ref(false)
const editingId = ref<string | null>(null)
const submitting = ref(false)
const formError = ref('')
const advancedOpen = ref(false)

const form = reactive({
  name: '',
  hosts: '',
  ports: '80, 443',
  backends: [{ url: 'http://127.0.0.1:8080', weight: 1, priority: 0, extra: {} }] as BackendDraft[],
  cacheEnabled: false,
  cacheTtl: 60,
  cacheTypes: '',
  cacheExtra: {} as Record<string, unknown>,
  healthEnabled: true,
  healthPath: '/',
  healthInterval: 10,
  healthTimeout: 2,
  unhealthyThreshold: 2,
  healthyThreshold: 2,
  healthExtra: {} as Record<string, unknown>,
  extra: '{}',
})

const canAdmin = computed(() => ['admin', 'service'].includes(session.user?.role ?? ''))

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

function cacheEnabled(item: Website): boolean {
  const config = item.config as Record<string, unknown> | null
  const cache = config?.cache as Record<string, unknown> | undefined
  return Boolean(cache?.enabled)
}

function healthEnabled(item: Website): boolean {
  const config = item.config as Record<string, unknown> | null
  const health = config?.health_check as Record<string, unknown> | undefined
  return Boolean(health?.enabled)
}

function parseHosts(value: string): string[] {
  return value
    .split(/[\s,]+/)
    .map((host) => host.trim())
    .filter(Boolean)
}

function parsePorts(value: string): number[] {
  return value
    .split(/[\s,]+/)
    .map((port) => port.trim())
    .filter(Boolean)
    .map((port) => {
      const parsed = Number(port)
      if (!Number.isInteger(parsed) || parsed < 1 || parsed > 65535) {
        throw new Error(t('websites.invalidPort', { port }))
      }
      return parsed
    })
}

function parseExtra(): Record<string, unknown> {
  try {
    const value = JSON.parse(form.extra || '{}') as unknown
    if (!value || typeof value !== 'object' || Array.isArray(value)) {
      throw new Error('not an object')
    }
    return value as Record<string, unknown>
  } catch {
    throw new Error(t('websites.invalidJson', { label: t('websites.advanced') }))
  }
}

function resetForm(): void {
  form.name = ''
  form.hosts = ''
  form.ports = '80, 443'
  form.backends = [{ url: 'http://127.0.0.1:8080', weight: 1, priority: 0, extra: {} }]
  form.cacheEnabled = false
  form.cacheTtl = 60
  form.cacheTypes = ''
  form.cacheExtra = {}
  form.healthEnabled = true
  form.healthPath = '/'
  form.healthInterval = 10
  form.healthTimeout = 2
  form.unhealthyThreshold = 2
  form.healthyThreshold = 2
  form.healthExtra = {}
  form.extra = '{}'
  advancedOpen.value = false
  formError.value = ''
}

function openCreate(): void {
  editingId.value = null
  resetForm()
  showForm.value = true
}

function openEdit(item: Website): void {
  editingId.value = item.id
  resetForm()
  form.name = item.name ?? ''
  form.hosts = item.hosts.join(', ')
  form.ports = item.ports.join(', ')

  if (Array.isArray(item.backends)) {
    const backends = item.backends as Array<Record<string, unknown>>
    form.backends = backends.map((backend) => {
      const { url, weight, priority, ...rest } = backend
      return {
        url: String(url ?? ''),
        weight: Number(weight ?? 1) || 1,
        priority: Number(priority ?? 0) || 0,
        extra: rest,
      }
    })
  }

  const config = (item.config ?? {}) as Record<string, unknown>
  const cache = (config.cache ?? {}) as Record<string, unknown>
  const health = (config.health_check ?? {}) as Record<string, unknown>
  form.cacheEnabled = Boolean(cache.enabled)
  form.cacheTtl = Number(cache.ttl ?? 60) || 60
  form.cacheTypes = Array.isArray(cache.content_types)
    ? cache.content_types.map(String).join(', ')
    : ''
  const cacheExtra = { ...cache }
  delete cacheExtra.enabled
  delete cacheExtra.ttl
  delete cacheExtra.content_types
  form.cacheExtra = cacheExtra
  // Sites created before the health_check config existed should default to
  // active probes; an explicit enabled:false is still respected.
  form.healthEnabled = health.enabled === undefined ? true : Boolean(health.enabled)
  form.healthPath = String(health.path ?? '/')
  form.healthInterval = Number(health.interval_secs ?? 10) || 10
  form.healthTimeout = Number(health.timeout_secs ?? 2) || 2
  form.unhealthyThreshold = Number(health.unhealthy_threshold ?? 2) || 2
  form.healthyThreshold = Number(health.healthy_threshold ?? 2) || 2
  const healthExtra = { ...health }
  delete healthExtra.enabled
  delete healthExtra.path
  delete healthExtra.interval_secs
  delete healthExtra.timeout_secs
  delete healthExtra.unhealthy_threshold
  delete healthExtra.healthy_threshold
  form.healthExtra = healthExtra

  const extra = { ...config }
  delete extra.cache
  delete extra.health_check
  form.extra = JSON.stringify(extra, null, 2)
  showForm.value = true
}

function addBackend(): void {
  form.backends.push({ url: '', weight: 1, priority: 0, extra: {} })
}

function removeBackend(index: number): void {
  form.backends.splice(index, 1)
}

async function submit(): Promise<void> {
  formError.value = ''
  submitting.value = true
  try {
    const hosts = parseHosts(form.hosts)
    const ports = parsePorts(form.ports)
    if (hosts.length === 0) {
      throw new Error(t('websites.hosts'))
    }
    if (form.backends.length === 0) {
      throw new Error(t('websites.noBackends'))
    }
    const backends = form.backends.map((backend) => {
      const url = backend.url.trim()
      if (!/^https?:\/\//i.test(url)) {
        throw new Error(t('websites.invalidBackend'))
      }
      return {
        ...backend.extra,
        url,
        weight: Math.max(1, Math.round(backend.weight) || 1),
        ...(backend.priority > 0 ? { priority: Math.round(backend.priority) } : {}),
      }
    })

    const config = parseExtra()
    if (form.cacheEnabled) {
      config.cache = {
        ...form.cacheExtra,
        enabled: true,
        ttl: Math.max(1, Math.round(form.cacheTtl) || 60),
        content_types: form.cacheTypes
          .split(',')
          .map((value) => value.trim())
          .filter(Boolean),
      }
    }
    if (form.healthEnabled) {
      config.health_check = {
        ...form.healthExtra,
        enabled: true,
        path: form.healthPath.trim() || '/',
        interval_secs: Math.max(1, Math.round(form.healthInterval) || 10),
        timeout_secs: Math.max(1, Math.round(form.healthTimeout) || 2),
        unhealthy_threshold: Math.max(1, Math.round(form.unhealthyThreshold) || 2),
        healthy_threshold: Math.max(1, Math.round(form.healthyThreshold) || 2),
      }
    }

    const payload: WebsiteInput = {
      name: form.name.trim() || undefined,
      hosts,
      ports,
      backends,
      config,
    }
    if (editingId.value) {
      await api.updateWebsite(editingId.value, payload)
    } else {
      await api.createWebsite(payload)
    }
    notifySuccess(t('websites.saved'))
    showForm.value = false
    await load()
  } catch (reason) {
    formError.value = errorMessage(reason)
  } finally {
    submitting.value = false
  }
}

async function remove(item: Website): Promise<void> {
  if (!window.confirm(t('websites.deleteConfirm', { name: item.name || item.id }))) return
  error.value = ''
  try {
    await api.deleteWebsite(item.id)
    await load()
  } catch (reason) {
    error.value = errorMessage(reason)
  }
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

usePolling(load)
</script>

<template>
  <div class="page">
    <header class="page-header">
      <div>
        <p class="eyebrow">{{ t('nav.websites') }}</p>
        <h2>{{ t('websites.title') }}</h2>
        <p class="page-subtitle">{{ t('websites.subtitle') }}</p>
      </div>
      <div class="button-row">
        <span class="pill-note">{{ t('websites.autoRefresh') }}</span>
        <button v-if="canAdmin" class="btn" type="button" @click="openCreate">
          <AppIcon name="plus" :size="16" />
          {{ t('websites.add') }}
        </button>
      </div>
    </header>

    <div v-if="error" class="error-banner">
      <AppIcon name="alert" :size="18" />
      <span>{{ error }}</span>
    </div>

    <div class="toolbar">
      <label class="search-box">
        <AppIcon name="search" :size="16" />
        <input v-model="search" type="search" :placeholder="t('websites.search')" />
      </label>
      <span class="pill-note">{{ canAdmin ? t('websites.adminMode') : t('common.readonly') }}</span>
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
              <th>{{ t('websites.cache') }} / {{ t('websites.health') }}</th>
              <th>{{ t('websites.updated') }}</th>
              <th v-if="canAdmin">{{ t('common.actions') }}</th>
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
              <td>
                <div class="chips">
                  <span v-if="cacheEnabled(item)" class="chip">{{ t('websites.cache') }}</span>
                  <span v-if="healthEnabled(item)" class="chip">{{ t('websites.health') }}</span>
                  <span v-if="!cacheEnabled(item) && !healthEnabled(item)" class="muted small-text"
                    >—</span
                  >
                </div>
              </td>
              <td class="muted">{{ formatDate(item.updated_at) }}</td>
              <td v-if="canAdmin">
                <div class="button-row">
                  <button class="btn small" type="button" @click="openEdit(item)">
                    {{ t('websites.edit') }}
                  </button>
                  <button class="btn small" type="button" @click="remove(item)">
                    {{ t('websites.delete') }}
                  </button>
                </div>
              </td>
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

    <AppModal
      v-if="showForm"
      :title="editingId ? t('websites.editTitle') : t('websites.createTitle')"
      wide
      @close="showForm = false"
    >
      <div class="form-section">
        <h4>{{ t('websites.basics') }}</h4>
        <p class="muted small-text">{{ t('websites.basicsHint') }}</p>
        <div class="form-grid">
          <label class="field">
            <span class="field-label">{{ t('websites.name') }}</span>
            <input v-model="form.name" class="input" :placeholder="t('websites.nameHint')" />
          </label>
          <label class="field">
            <span class="field-label">{{ t('websites.ports') }}</span>
            <input v-model="form.ports" class="input" :placeholder="t('websites.portsHint')" />
          </label>
          <label class="field field-wide">
            <span class="field-label">{{ t('websites.hosts') }}</span>
            <input v-model="form.hosts" class="input" :placeholder="t('websites.hostsHint')" />
          </label>
        </div>
      </div>

      <div class="form-section">
        <div class="section-head">
          <div>
            <h4>{{ t('websites.backends') }}</h4>
            <p class="muted small-text">{{ t('websites.backendsHint') }}</p>
          </div>
          <button class="btn small" type="button" @click="addBackend">
            <AppIcon name="plus" :size="14" />
            {{ t('websites.addBackend') }}
          </button>
        </div>
        <div class="backend-list">
          <div v-for="(backend, index) in form.backends" :key="index" class="backend-row">
            <label class="field backend-url">
              <span class="field-label">{{ t('websites.backendUrl') }}</span>
              <input v-model="backend.url" class="input" placeholder="http://127.0.0.1:8080" />
            </label>
            <label class="field backend-number">
              <span class="field-label">{{ t('websites.weight') }}</span>
              <input v-model.number="backend.weight" class="input" type="number" min="1" />
            </label>
            <label class="field backend-number">
              <span class="field-label">{{ t('websites.priority') }}</span>
              <input v-model.number="backend.priority" class="input" type="number" min="0" />
            </label>
            <button
              class="icon-btn backend-remove"
              type="button"
              :aria-label="t('websites.delete')"
              @click="removeBackend(index)"
            >
              <AppIcon name="close" :size="16" />
            </button>
          </div>
        </div>
      </div>

      <div class="form-columns">
        <div class="form-section">
          <div class="section-head">
            <div>
              <h4>{{ t('websites.cache') }}</h4>
            </div>
            <label class="switch">
              <input v-model="form.cacheEnabled" type="checkbox" />
              <span>{{ t('websites.cacheEnabled') }}</span>
            </label>
          </div>
          <div class="form-grid">
            <label class="field">
              <span class="field-label">{{ t('websites.cacheTtl') }}</span>
              <input v-model.number="form.cacheTtl" class="input" type="number" min="1" />
            </label>
            <label class="field">
              <span class="field-label">{{ t('websites.cacheTypes') }}</span>
              <input v-model="form.cacheTypes" class="input" placeholder="text/html, image/*" />
            </label>
          </div>
          <p class="muted small-text">{{ t('websites.cacheTypesHint') }}</p>
        </div>

        <div class="form-section">
          <div class="section-head">
            <div>
              <h4>{{ t('websites.health') }}</h4>
            </div>
            <label class="switch">
              <input v-model="form.healthEnabled" type="checkbox" />
              <span>{{ t('websites.healthEnabled') }}</span>
            </label>
          </div>
          <div class="form-grid">
            <label class="field">
              <span class="field-label">{{ t('websites.healthPath') }}</span>
              <input v-model="form.healthPath" class="input" placeholder="/" />
            </label>
            <label class="field">
              <span class="field-label">{{ t('websites.healthInterval') }}</span>
              <input v-model.number="form.healthInterval" class="input" type="number" min="1" />
            </label>
            <label class="field">
              <span class="field-label">{{ t('websites.healthTimeout') }}</span>
              <input v-model.number="form.healthTimeout" class="input" type="number" min="1" />
            </label>
            <label class="field">
              <span class="field-label">{{ t('websites.unhealthyThreshold') }}</span>
              <input v-model.number="form.unhealthyThreshold" class="input" type="number" min="1" />
            </label>
            <label class="field">
              <span class="field-label">{{ t('websites.healthyThreshold') }}</span>
              <input v-model.number="form.healthyThreshold" class="input" type="number" min="1" />
            </label>
          </div>
        </div>
      </div>

      <div class="form-section">
        <button class="advanced-toggle" type="button" @click="advancedOpen = !advancedOpen">
          <AppIcon :name="advancedOpen ? 'arrow' : 'plus'" :size="14" />
          {{ t('websites.advanced') }}
        </button>
        <template v-if="advancedOpen">
          <p class="muted small-text">{{ t('websites.advancedHint') }}</p>
          <textarea v-model="form.extra" class="input code" rows="8"></textarea>
        </template>
      </div>

      <div v-if="formError" class="form-error">{{ formError }}</div>

      <template #footer>
        <button class="btn" type="button" :disabled="submitting" @click="submit">
          {{ submitting ? t('websites.saving') : t('websites.save') }}
        </button>
        <button class="btn" type="button" @click="showForm = false">
          {{ t('certs.cancel') }}
        </button>
      </template>
    </AppModal>
  </div>
</template>
