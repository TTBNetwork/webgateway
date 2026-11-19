<script setup lang="ts">
import { onMounted, ref } from 'vue'
import AppIcon from '../components/AppIcon.vue'
import EmptyState from '../components/EmptyState.vue'
import StatusBadge from '../components/StatusBadge.vue'
import { api, type Certificate } from '../api'
import { daysUntil, errorMessage, formatDate } from '../format'
import { t } from '../i18n'

type CertStatus = 'pending' | 'valid' | 'expiring' | 'expired' | 'unknown'

const items = ref<Certificate[]>([])
const loading = ref(false)
const error = ref('')

const showForm = ref(false)
const mode = ref<'auto' | 'manual'>('auto')
const name = ref('')
const hosts = ref('')
const email = ref('')
const zone = ref('')
const fullchain = ref('')
const privateKey = ref('')
const submitting = ref(false)
const formError = ref('')
const formSuccess = ref('')

function certificateStatus(item: Certificate): CertStatus {
  if (item.status === 'pending') return 'pending'
  if (item.status === 'expired') return 'expired'
  if (item.status === 'expiring') return 'expiring'
  if (item.status === 'valid') return 'valid'
  const days = daysUntil(item.expires_at)
  if (days === null) return 'unknown'
  if (days < 0) return 'expired'
  if (days <= 30) return 'expiring'
  return 'valid'
}

function statusLabel(status: CertStatus): string {
  if (status === 'pending') return t('certs.pending')
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

function parseHosts(value: string): string[] {
  return value
    .split(/[\s,]+/)
    .map((host) => host.trim())
    .filter(Boolean)
}

function resetForm(): void {
  mode.value = 'auto'
  name.value = ''
  hosts.value = ''
  email.value = ''
  zone.value = ''
  fullchain.value = ''
  privateKey.value = ''
  formError.value = ''
  formSuccess.value = ''
}

function openForm(): void {
  resetForm()
  showForm.value = true
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

async function submit(): Promise<void> {
  formError.value = ''
  formSuccess.value = ''
  submitting.value = true
  try {
    const hostnames = parseHosts(hosts.value)
    if (mode.value === 'auto') {
      if (hostnames.length === 0) {
        throw new Error(t('certs.hostsRequired'))
      }
      await api.issueCertificate({
        name: name.value.trim() || undefined,
        hostnames,
        email: email.value.trim() || undefined,
        dns_zone: zone.value.trim() || undefined,
      })
      formSuccess.value = t('certs.queued')
    } else {
      if (!fullchain.value.trim() || !privateKey.value.trim()) {
        throw new Error(t('certs.pemRequired'))
      }
      await api.uploadCertificate({
        name: name.value.trim() || undefined,
        hostnames: hostnames.length ? hostnames : undefined,
        fullchain: fullchain.value,
        private_key: privateKey.value,
      })
      formSuccess.value = t('certs.uploaded')
    }
    fullchain.value = ''
    privateKey.value = ''
    await load()
  } catch (reason) {
    formError.value = errorMessage(reason)
  } finally {
    submitting.value = false
  }
}

async function reissue(item: Certificate): Promise<void> {
  error.value = ''
  try {
    await api.reissueCertificate(item.id)
    await load()
  } catch (reason) {
    error.value = errorMessage(reason)
  }
}

async function remove(item: Certificate): Promise<void> {
  if (!window.confirm(t('certs.deleteConfirm'))) return
  error.value = ''
  try {
    await api.deleteCertificate(item.id)
    await load()
  } catch (reason) {
    error.value = errorMessage(reason)
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
      <div class="button-row">
        <button class="btn" type="button" @click="openForm">
          <AppIcon name="certificates" :size="16" />
          {{ t('certs.add') }}
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
      <div class="button-row">
        <button
          class="segment"
          :class="{ active: mode === 'auto' }"
          type="button"
          @click="mode = 'auto'"
        >
          {{ t('certs.auto') }}
        </button>
        <button
          class="segment"
          :class="{ active: mode === 'manual' }"
          type="button"
          @click="mode = 'manual'"
        >
          {{ t('certs.manual') }}
        </button>
      </div>
      <p class="muted small-text">
        {{ mode === 'auto' ? t('certs.autoHint') : t('certs.manualHint') }}
      </p>

      <div style="display: grid; gap: 6px">
        <label class="field-label">{{ t('certs.name') }}</label>
        <input v-model="name" class="input" :placeholder="t('certs.nameHint')" />
      </div>

      <div style="display: grid; gap: 6px">
        <label class="field-label">{{ t('certs.hosts') }}</label>
        <input v-model="hosts" class="input" :placeholder="t('certs.hostsHint')" />
      </div>

      <template v-if="mode === 'auto'">
        <div style="display: grid; gap: 6px">
          <label class="field-label">{{ t('certs.email') }}</label>
          <input v-model="email" class="input" placeholder="admin@example.com" />
        </div>
        <div style="display: grid; gap: 6px">
          <label class="field-label">{{ t('certs.zone') }}</label>
          <input v-model="zone" class="input" :placeholder="t('certs.zoneHint')" />
        </div>
      </template>

      <template v-else>
        <div style="display: grid; gap: 6px">
          <label class="field-label">{{ t('certs.fullchain') }}</label>
          <textarea
            v-model="fullchain"
            class="input"
            rows="6"
            style="font-family: ui-monospace, monospace"
            placeholder="-----BEGIN CERTIFICATE-----"
          ></textarea>
        </div>
        <div style="display: grid; gap: 6px">
          <label class="field-label">{{ t('certs.privateKey') }}</label>
          <textarea
            v-model="privateKey"
            class="input"
            rows="6"
            style="font-family: ui-monospace, monospace"
            placeholder="-----BEGIN PRIVATE KEY-----"
          ></textarea>
        </div>
      </template>

      <div v-if="formError" class="form-error">{{ formError }}</div>
      <div v-if="formSuccess" class="form-success">{{ formSuccess }}</div>

      <div class="button-row">
        <button class="btn" type="button" :disabled="submitting" @click="submit">
          {{ submitting ? t('certs.submitting') : t('certs.submit') }}
        </button>
        <button class="btn small" type="button" @click="showForm = false">
          {{ t('certs.cancel') }}
        </button>
      </div>
    </section>

    <div v-if="loading && items.length === 0" class="loading">{{ t('common.loading') }}…</div>

    <div v-else-if="items.length" class="card table-card">
      <div class="table-wrap">
        <table>
          <thead>
            <tr>
              <th>{{ t('certs.hostnames') }}</th>
              <th>{{ t('certs.status') }}</th>
              <th>{{ t('certs.expires') }}</th>
              <th>{{ t('common.actions') }}</th>
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
                    <div class="muted small-text">
                      {{ item.auto_renew ? t('certs.auto') : t('certs.manual') }}
                      <span v-if="item.dns_zone"> · {{ item.dns_zone }}</span>
                    </div>
                  </div>
                </div>
              </td>
              <td>
                <StatusBadge
                  :status="
                    certificateStatus(item) === 'pending' ? 'pending' : certificateStatus(item)
                  "
                  :label="statusLabel(certificateStatus(item))"
                />
                <div class="muted small-text">{{ expiryHint(item.expires_at) }}</div>
                <div v-if="item.last_error" class="form-error" style="margin-top: 6px">
                  {{ item.last_error }}
                </div>
              </td>
              <td class="muted">
                {{ formatDate(item.expires_at) }}
                <div class="small-text">{{ formatDate(item.created_at) }}</div>
              </td>
              <td>
                <div class="button-row">
                  <button
                    v-if="item.auto_renew"
                    class="btn small"
                    type="button"
                    @click="reissue(item)"
                  >
                    {{ t('certs.reissue') }}
                  </button>
                  <button class="btn small" type="button" @click="remove(item)">
                    {{ t('certs.delete') }}
                  </button>
                </div>
              </td>
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
