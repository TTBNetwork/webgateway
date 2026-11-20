<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import AppIcon from '../components/AppIcon.vue'
import { api } from '../api'
import { errorMessage } from '../format'
import { t } from '../i18n'
import { prefs } from '../prefs'
import { navigate } from '../router'
import { clearSession, session } from '../session'
import { notifySuccess } from '../toast'

const canAdmin = computed(() => ['admin', 'service'].includes(session.user?.role ?? ''))
const proxyUrl = ref('')
const proxySaving = ref(false)
const proxyError = ref('')

async function loadProxy(): Promise<void> {
  if (!canAdmin.value) return
  try {
    proxyUrl.value = (await api.getAcmeProxy()).url ?? ''
  } catch (reason) {
    proxyError.value = t('settings.acmeProxyLoadFailed') + ': ' + errorMessage(reason)
  }
}

async function saveProxy(): Promise<void> {
  proxySaving.value = true
  proxyError.value = ''
  try {
    const value = proxyUrl.value.trim()
    const result = await api.updateAcmeProxy(value || null)
    proxyUrl.value = result.url ?? ''
    notifySuccess(value ? t('settings.acmeProxySaved') : t('settings.acmeProxyCleared'))
  } catch (reason) {
    proxyError.value = errorMessage(reason)
  } finally {
    proxySaving.value = false
  }
}

onMounted(loadProxy)

function signOut(): void {
  clearSession()
  navigate('/login')
}

function setTheme(theme: 'dark' | 'light'): void {
  prefs.theme = theme
}

function setLocale(locale: 'zh' | 'en'): void {
  prefs.locale = locale
}
</script>

<template>
  <div class="page">
    <header class="page-header">
      <div>
        <p class="eyebrow">{{ t('nav.settings') }}</p>
        <h2>{{ t('settings.title') }}</h2>
        <p class="page-subtitle">{{ t('settings.subtitle') }}</p>
      </div>
    </header>

    <div class="settings-grid">
      <section class="card setting-card">
        <div class="card-header">
          <div>
            <h3>{{ t('settings.appearance') }}</h3>
            <p>{{ t('settings.theme') }} / {{ t('settings.language') }}</p>
          </div>
        </div>

        <div class="setting-row">
          <span>{{ t('settings.theme') }}</span>
          <div class="segmented">
            <button
              type="button"
              class="segment"
              :class="{ active: prefs.theme === 'dark' }"
              @click="setTheme('dark')"
            >
              {{ t('common.dark') }}
            </button>
            <button
              type="button"
              class="segment"
              :class="{ active: prefs.theme === 'light' }"
              @click="setTheme('light')"
            >
              {{ t('common.light') }}
            </button>
          </div>
        </div>

        <div class="setting-row">
          <span>{{ t('settings.language') }}</span>
          <div class="segmented">
            <button
              type="button"
              class="segment"
              :class="{ active: prefs.locale === 'zh' }"
              @click="setLocale('zh')"
            >
              中文
            </button>
            <button
              type="button"
              class="segment"
              :class="{ active: prefs.locale === 'en' }"
              @click="setLocale('en')"
            >
              English
            </button>
          </div>
        </div>
      </section>

      <section class="card setting-card">
        <div class="card-header">
          <div>
            <h3>{{ t('settings.security') }}</h3>
            <p>{{ t('settings.account') }} / {{ t('settings.role') }}</p>
          </div>
        </div>

        <dl class="kv">
          <div>
            <dt>{{ t('settings.account') }}</dt>
            <dd>{{ session.user?.username ?? '—' }}</dd>
          </div>
          <div>
            <dt>{{ t('settings.role') }}</dt>
            <dd class="mono">{{ session.user?.role ?? '—' }}</dd>
          </div>
        </dl>

        <p class="small-text muted">{{ t('settings.tokenHint') }}</p>

        <div class="button-row">
          <button class="btn danger-ghost" type="button" @click="signOut">
            <AppIcon name="logout" :size="16" />
            {{ t('settings.signOut') }}
          </button>
        </div>
      </section>

      <section v-if="canAdmin" class="card setting-card">
        <div class="card-header">
          <div>
            <h3>{{ t('settings.acmeProxy') }}</h3>
            <p>{{ t('settings.acmeProxyHint') }}</p>
          </div>
        </div>

        <div class="proxy-form">
          <input
            v-model="proxyUrl"
            class="input"
            :placeholder="t('settings.acmeProxyPlaceholder')"
            autocomplete="off"
          />
          <button class="btn" type="button" :disabled="proxySaving" @click="saveProxy">
            {{ proxySaving ? t('websites.saving') : t('common.save') }}
          </button>
        </div>
        <p v-if="proxyError" class="form-error">{{ proxyError }}</p>
        <p v-else class="muted small-text">{{ t('settings.acmeProxyAdmin') }}</p>
      </section>

      <section class="card setting-card">
        <div class="card-header">
          <div>
            <h3>{{ t('settings.about') }}</h3>
            <p>{{ t('app.footer') }}</p>
          </div>
        </div>

        <dl class="kv">
          <div>
            <dt>{{ t('settings.backend') }}</dt>
            <dd class="mono">/api</dd>
          </div>
          <div>
            <dt>{{ t('settings.schema') }}</dt>
            <dd class="mono">v6</dd>
          </div>
          <div>
            <dt>{{ t('settings.version') }}</dt>
            <dd class="mono">0.1.0</dd>
          </div>
          <div>
            <dt>{{ t('settings.token') }}</dt>
            <dd>{{ session.user ? t('settings.tokenStored') : t('settings.tokenMissing') }}</dd>
          </div>
        </dl>
      </section>
    </div>
  </div>
</template>
