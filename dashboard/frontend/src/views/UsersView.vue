<script setup lang="ts">
import { onMounted, ref } from 'vue'
import AppIcon from '../components/AppIcon.vue'
import EmptyState from '../components/EmptyState.vue'
import StatusBadge from '../components/StatusBadge.vue'
import { api, type UserSummary } from '../api'
import { errorMessage, formatDate } from '../format'
import { t } from '../i18n'
import { session } from '../session'

const items = ref<UserSummary[]>([])
const loading = ref(false)
const error = ref('')

const showForm = ref(false)
const username = ref('')
const role = ref('user')
const submitting = ref(false)
const formError = ref('')
const newSecret = ref('')
const newSecretUser = ref('')

const roles = ['admin', 'user', 'view']

async function load(): Promise<void> {
  loading.value = true
  error.value = ''
  try {
    items.value = await api.users()
  } catch (reason) {
    error.value = errorMessage(reason)
  } finally {
    loading.value = false
  }
}

function openForm(): void {
  username.value = ''
  role.value = 'user'
  formError.value = ''
  newSecret.value = ''
  newSecretUser.value = ''
  showForm.value = true
}

async function create(): Promise<void> {
  formError.value = ''
  submitting.value = true
  try {
    const result = await api.createUser(username.value.trim(), role.value)
    newSecret.value = result.totp_secret
    newSecretUser.value = result.user.username
    showForm.value = false
    await load()
  } catch (reason) {
    formError.value = errorMessage(reason)
  } finally {
    submitting.value = false
  }
}

function onRoleChange(user: UserSummary, event: Event): void {
  const value = (event.target as HTMLSelectElement).value
  void updateRole(user, value)
}

async function updateRole(user: UserSummary, role: string): Promise<void> {
  if (role === user.role) return
  error.value = ''
  try {
    await api.updateUser(user.id, { role })
    await load()
  } catch (reason) {
    error.value = errorMessage(reason)
    await load()
  }
}

async function resetTotp(user: UserSummary): Promise<void> {
  if (!window.confirm(t('users.resetConfirm', { username: user.username }))) return
  error.value = ''
  try {
    const result = await api.resetUserTotp(user.id)
    newSecret.value = result.totp_secret
    newSecretUser.value = user.username
  } catch (reason) {
    error.value = errorMessage(reason)
  }
}

async function revoke(user: UserSummary): Promise<void> {
  if (!window.confirm(t('users.revokeConfirm', { username: user.username }))) return
  error.value = ''
  try {
    await api.revokeUser(user.id)
    error.value = ''
  } catch (reason) {
    error.value = errorMessage(reason)
  }
}

async function remove(user: UserSummary): Promise<void> {
  if (!window.confirm(t('users.deleteConfirm', { username: user.username }))) return
  error.value = ''
  try {
    await api.deleteUser(user.id)
    await load()
  } catch (reason) {
    error.value = errorMessage(reason)
  }
}

function isSelf(user: UserSummary): boolean {
  return session.user?.id === user.id
}

onMounted(load)
</script>

<template>
  <div class="page">
    <header class="page-header">
      <div>
        <p class="eyebrow">{{ t('nav.users') }}</p>
        <h2>{{ t('users.title') }}</h2>
        <p class="page-subtitle">{{ t('users.subtitle') }}</p>
      </div>
      <div class="button-row">
        <button class="btn" type="button" @click="openForm">
          <AppIcon name="plus" :size="16" />
          {{ t('users.add') }}
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

    <div v-if="newSecret" class="card" style="margin-bottom: 18px">
      <div class="form-success" style="margin-bottom: 8px">
        {{ t('users.secretOnce', { username: newSecretUser }) }}
      </div>
      <code style="display: block; padding: 10px; font-size: 13px; word-break: break-all">
        {{ newSecret }}
      </code>
      <button class="btn small" type="button" style="margin-top: 10px" @click="newSecret = ''">
        {{ t('common.close') }}
      </button>
    </div>

    <section v-if="showForm" class="card" style="display: grid; gap: 14px; margin-bottom: 18px">
      <div style="display: grid; gap: 6px">
        <label class="field-label">{{ t('users.username') }}</label>
        <input v-model="username" class="input" :placeholder="t('users.usernameHint')" />
      </div>
      <div style="display: grid; gap: 6px">
        <label class="field-label">{{ t('users.role') }}</label>
        <select v-model="role" class="input">
          <option v-for="item in roles" :key="item" :value="item">{{ item }}</option>
        </select>
      </div>
      <div v-if="formError" class="form-error">{{ formError }}</div>
      <div class="button-row">
        <button class="btn" type="button" :disabled="submitting" @click="create">
          {{ submitting ? t('users.creating') : t('users.create') }}
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
              <th>{{ t('users.username') }}</th>
              <th>{{ t('users.role') }}</th>
              <th>{{ t('users.lastLogin') }}</th>
              <th>{{ t('common.actions') }}</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="user in items" :key="user.id">
              <td>
                <div class="entity">
                  <div class="entity-icon"><AppIcon name="users" :size="18" /></div>
                  <div>
                    <strong>{{ user.username }}</strong>
                    <div class="muted small-text">
                      {{ user.id }} · {{ t('users.tokenVersion') }} {{ user.token_version }}
                    </div>
                  </div>
                </div>
              </td>
              <td>
                <select
                  class="input"
                  :value="user.role"
                  :disabled="isSelf(user)"
                  @change="onRoleChange(user, $event)"
                >
                  <option v-for="item in roles" :key="item" :value="item">{{ item }}</option>
                </select>
                <StatusBadge
                  :status="user.role === 'admin' ? 'online' : 'info'"
                  :label="user.role"
                />
              </td>
              <td class="muted">
                {{ formatDate(user.last_login) }}
                <div class="small-text">{{ formatDate(user.created_at) }}</div>
              </td>
              <td>
                <div class="button-row">
                  <button class="btn small" type="button" @click="resetTotp(user)">
                    {{ t('users.resetTotp') }}
                  </button>
                  <button class="btn small" type="button" @click="revoke(user)">
                    {{ t('users.revoke') }}
                  </button>
                  <button
                    class="btn small"
                    type="button"
                    :disabled="isSelf(user)"
                    @click="remove(user)"
                  >
                    {{ t('users.delete') }}
                  </button>
                </div>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
      <p class="card-footnote">{{ t('users.footnote') }}</p>
    </div>

    <EmptyState
      v-else-if="!loading"
      icon="users"
      :title="t('users.empty')"
      :hint="t('users.emptyHint')"
    />
  </div>
</template>
