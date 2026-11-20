<script setup lang="ts">
import { computed, ref } from 'vue'
import AppIcon from '../components/AppIcon.vue'
import EmptyState from '../components/EmptyState.vue'
import AppModal from '../components/AppModal.vue'
import SelectField from '../components/SelectField.vue'
import { api, type DnsProvider } from '../api'
import { usePolling } from '../composables/usePolling'
import { errorMessage } from '../format'
import { t } from '../i18n'
import { session } from '../session'
import { notifySuccess } from '../toast'

const items = ref<DnsProvider[]>([])
const loading = ref(false)
const error = ref('')

const showForm = ref(false)
const name = ref('')
const domains = ref('')
const kind = ref('tencent')
const secretId = ref('')
const secretKey = ref('')
const submitting = ref(false)
const formError = ref('')

const canAdmin = computed(() => ['admin', 'service'].includes(session.user?.role ?? ''))
const kindOptions = computed(() => [{ value: 'tencent', label: t('dns.tencent') }])

async function load(): Promise<void> {
  loading.value = true
  error.value = ''
  try {
    items.value = await api.dnsProviders()
  } catch (reason) {
    error.value = errorMessage(reason)
  } finally {
    loading.value = false
  }
}

usePolling(load, 30_000)

function openCreate(): void {
  name.value = ''
  domains.value = ''
  kind.value = 'tencent'
  secretId.value = ''
  secretKey.value = ''
  formError.value = ''
  showForm.value = true
}

async function submit(): Promise<void> {
  formError.value = ''
  const domainList = domains.value
    .split(/[\s,]+/)
    .map((value) => value.trim())
    .filter(Boolean)
  if (domainList.length === 0) {
    formError.value = t('dns.required')
    return
  }
  submitting.value = true
  try {
    await api.createDnsProvider({
      name: name.value.trim() || undefined,
      domains: domainList,
      kind: kind.value,
      from_env: false,
      secret_id: secretId.value.trim() || undefined,
      secret_key: secretKey.value.trim() || undefined,
    })
    showForm.value = false
    notifySuccess(t('dns.created'))
    await load()
  } catch (reason) {
    formError.value = errorMessage(reason)
  } finally {
    submitting.value = false
  }
}

async function remove(item: DnsProvider): Promise<void> {
  const label = item.name || item.domains.join(', ') || item.id
  if (!window.confirm(t('dns.deleteConfirm', { name: label }))) return
  try {
    await api.deleteDnsProvider(item.id)
    notifySuccess(t('dns.deleted'))
    await load()
  } catch (reason) {
    error.value = errorMessage(reason)
  }
}
</script>

<template>
  <div class="page">
    <header class="page-header">
      <div>
        <p class="eyebrow">{{ t('nav.dns') }}</p>
        <h2>{{ t('dns.title') }}</h2>
        <p class="page-subtitle">{{ t('dns.subtitle') }}</p>
      </div>
      <button v-if="canAdmin" class="btn" type="button" @click="openCreate">
        <AppIcon name="plus" :size="16" />
        {{ t('dns.add') }}
      </button>
    </header>

    <div v-if="error" class="error-banner">
      <AppIcon name="alert" :size="18" />
      <span>{{ error }}</span>
    </div>

    <div v-if="loading && items.length === 0" class="loading">{{ t('common.loading') }}…</div>

    <div v-else-if="items.length" class="card table-card">
      <div class="table-wrap">
        <table>
          <thead>
            <tr>
              <th>{{ t('dns.name') }}</th>
              <th>{{ t('dns.kind') }}</th>
              <th>{{ t('dns.domains') }}</th>
              <th>{{ t('common.actions') }}</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="item in items" :key="item.id">
              <td>
                <div class="entity">
                  <div class="entity-icon"><AppIcon name="dns" :size="18" /></div>
                  <div>
                    <strong>{{ item.name || item.id }}</strong>
                    <div class="muted small-text mono">{{ item.id }}</div>
                  </div>
                </div>
              </td>
              <td>
                <span class="badge badge-info">{{ item.kind }}</span>
                <div v-if="item.from_env" class="muted small-text">{{ t('dns.fromEnv') }}</div>
              </td>
              <td>
                <div class="chips">
                  <span v-for="domain in item.domains" :key="domain" class="chip">{{
                    domain
                  }}</span>
                </div>
              </td>
              <td>
                <button v-if="canAdmin" class="btn small" type="button" @click="remove(item)">
                  {{ t('dns.delete') }}
                </button>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </div>

    <EmptyState
      v-else-if="!loading"
      icon="dns"
      :title="t('dns.empty')"
      :hint="t('dns.emptyHint')"
    />

    <AppModal v-if="showForm" :title="t('dns.add')" @close="showForm = false">
      <div class="form-grid">
        <label class="field">
          <span class="field-label">{{ t('dns.name') }}</span>
          <input v-model="name" class="input" :placeholder="t('dns.nameHint')" />
        </label>
        <label class="field">
          <span class="field-label">{{ t('dns.kind') }}</span>
          <SelectField v-model="kind" :options="kindOptions" />
        </label>
        <label class="field field-wide">
          <span class="field-label">{{ t('dns.domains') }}</span>
          <input v-model="domains" class="input" :placeholder="t('dns.domainsHint')" />
        </label>
        <label class="field">
          <span class="field-label">{{ t('dns.secretId') }}</span>
          <input v-model="secretId" class="input" autocomplete="off" />
        </label>
        <label class="field">
          <span class="field-label">{{ t('dns.secretKey') }}</span>
          <input v-model="secretKey" class="input" type="password" autocomplete="new-password" />
        </label>
        <p class="muted small-text field-wide">{{ t('dns.secretHint') }}</p>
        <div v-if="formError" class="form-error field-wide">{{ formError }}</div>
      </div>
      <template #footer>
        <button class="btn" type="button" :disabled="submitting" @click="submit">
          {{ submitting ? t('dns.creating') : t('dns.create') }}
        </button>
        <button class="btn" type="button" @click="showForm = false">{{ t('certs.cancel') }}</button>
      </template>
    </AppModal>
  </div>
</template>
