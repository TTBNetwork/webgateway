<script setup lang="ts">
import { ref } from 'vue'
import AppIcon from '../components/AppIcon.vue'
import { t } from '../i18n'
import { api } from '../api'
import { errorMessage } from '../format'
import { prefs } from '../prefs'
import { navigate } from '../router'
import { clearToken, session, setToken } from '../session'

const tokenDraft = ref(session.token)
const message = ref('')
const error = ref('')
const saving = ref(false)

async function saveToken(): Promise<void> {
  saving.value = true
  message.value = ''
  error.value = ''
  setToken(tokenDraft.value.trim())
  try {
    await api.summary()
    message.value = t('settings.tokenSaved')
  } catch (reason) {
    clearToken()
    error.value = errorMessage(reason)
  } finally {
    saving.value = false
  }
}

function signOut(): void {
  clearToken()
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
            <p>{{ t('settings.token') }}</p>
          </div>
        </div>

        <label class="field-label" for="settings-token">{{ t('settings.token') }}</label>
        <input
          id="settings-token"
          v-model="tokenDraft"
          class="input"
          type="password"
          :placeholder="t('login.placeholder')"
        />
        <p class="small-text muted">{{ t('settings.tokenHint') }}</p>

        <p v-if="message" class="form-success">{{ message }}</p>
        <p v-if="error" class="form-error">{{ error }}</p>

        <div class="button-row">
          <button class="btn primary" type="button" :disabled="saving" @click="saveToken">
            <AppIcon name="check" :size="16" />
            {{ saving ? t('common.loading') : t('settings.saveToken') }}
          </button>
          <button class="btn danger-ghost" type="button" @click="signOut">
            <AppIcon name="logout" :size="16" />
            {{ t('settings.signOut') }}
          </button>
        </div>
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
            <dd class="mono">v2</dd>
          </div>
          <div>
            <dt>{{ t('settings.version') }}</dt>
            <dd class="mono">0.1.0</dd>
          </div>
          <div>
            <dt>{{ t('settings.token') }}</dt>
            <dd>{{ session.token ? t('settings.tokenStored') : t('settings.tokenMissing') }}</dd>
          </div>
        </dl>
      </section>
    </div>
  </div>
</template>
