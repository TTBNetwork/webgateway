<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import AppIcon from '../components/AppIcon.vue'
import EmptyState from '../components/EmptyState.vue'
import { api, type Website, type WebsiteInput } from '../api'
import { errorMessage, formatDate } from '../format'
import { t } from '../i18n'
import { session } from '../session'

const items = ref<Website[]>([])
const loading = ref(false)
const error = ref('')
const search = ref('')

const showForm = ref(false)
const editingId = ref<string | null>(null)
const name = ref('')
const hostsText = ref('')
const portsText = ref('')
const backendsText = ref('')
const configText = ref('')
const submitting = ref(false)
const formError = ref('')
const formSuccess = ref('')

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

function parseJson(value: string, label: string): unknown {
  try {
    return JSON.parse(value)
  } catch {
    throw new Error(t('websites.invalidJson', { label }))
  }
}

function openCreate(): void {
  editingId.value = null
  name.value = ''
  hostsText.value = ''
  portsText.value = ''
  backendsText.value = '[\n  { "url": "http://127.0.0.1:8080" }\n]'
  configText.value = '{}'
  formError.value = ''
  formSuccess.value = ''
  showForm.value = true
}

function openEdit(item: Website): void {
  editingId.value = item.id
  name.value = item.name ?? ''
  hostsText.value = item.hosts.join(', ')
  portsText.value = item.ports.join(', ')
  backendsText.value = JSON.stringify(item.backends, null, 2)
  configText.value = JSON.stringify(item.config, null, 2)
  formError.value = ''
  formSuccess.value = ''
  showForm.value = true
}

async function submit(): Promise<void> {
  formError.value = ''
  submitting.value = true
  try {
    const payload: WebsiteInput = {
      name: name.value.trim() || undefined,
      hosts: parseHosts(hostsText.value),
      ports: parsePorts(portsText.value),
      backends: parseJson(backendsText.value || '[]', 'backends'),
      config: parseJson(configText.value || '{}', 'config'),
    }
    if (editingId.value) {
      await api.updateWebsite(editingId.value, payload)
    } else {
      await api.createWebsite(payload)
    }
    formSuccess.value = t('websites.saved')
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
      <div class="button-row">
        <button v-if="canAdmin" class="btn" type="button" @click="openCreate">
          <AppIcon name="plus" :size="16" />
          {{ t('websites.add') }}
        </button>
        <button class="btn" type="button" :disabled="loading" @click="load">
          <AppIcon name="refresh" :size="16" />
          {{ t('common.refresh') }}
        </button>
      </div>
    </header>

    <div v-if="error" class="error-banner">
      <AppIcon name="alert" :size="18" />
      <span>{{ error }}</span>
      <button class="btn small" type="button" @click="load">{{ t('common.retry') }}</button>
    </div>

    <section v-if="showForm" class="card" style="display: grid; gap: 14px; margin-bottom: 18px">
      <div style="display: grid; gap: 6px">
        <label class="field-label">{{ t('websites.name') }}</label>
        <input v-model="name" class="input" :placeholder="t('websites.nameHint')" />
      </div>
      <div style="display: grid; gap: 6px">
        <label class="field-label">{{ t('websites.hosts') }}</label>
        <input v-model="hostsText" class="input" :placeholder="t('websites.hostsHint')" />
      </div>
      <div style="display: grid; gap: 6px">
        <label class="field-label">{{ t('websites.ports') }}</label>
        <input v-model="portsText" class="input" :placeholder="t('websites.portsHint')" />
      </div>
      <div style="display: grid; gap: 6px">
        <label class="field-label">{{ t('websites.backends') }}</label>
        <textarea
          v-model="backendsText"
          class="input"
          rows="6"
          style="font-family: ui-monospace, monospace"
        ></textarea>
      </div>
      <div style="display: grid; gap: 6px">
        <label class="field-label">{{ t('websites.config') }}</label>
        <textarea
          v-model="configText"
          class="input"
          rows="4"
          style="font-family: ui-monospace, monospace"
        ></textarea>
      </div>
      <div v-if="formError" class="form-error">{{ formError }}</div>
      <div v-if="formSuccess" class="form-success">{{ formSuccess }}</div>
      <div class="button-row">
        <button class="btn" type="button" :disabled="submitting" @click="submit">
          {{ submitting ? t('websites.saving') : t('websites.save') }}
        </button>
        <button class="btn small" type="button" @click="showForm = false">
          {{ t('certs.cancel') }}
        </button>
      </div>
    </section>

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
  </div>
</template>
