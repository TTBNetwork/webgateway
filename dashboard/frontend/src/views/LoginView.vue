<script setup lang="ts">
import { ref } from 'vue'
import AppIcon from '../components/AppIcon.vue'
import InfoTip from '../components/InfoTip.vue'
import { api } from '../api'
import { errorMessage } from '../format'
import { t } from '../i18n'
import { prefs } from '../prefs'
import { navigate } from '../router'
import { session, setSession } from '../session'
import { notifyError } from '../toast'

const username = ref(session.user?.username ?? 'admin')
const code = ref('')
const loading = ref(false)

async function submit(): Promise<void> {
  if (!username.value.trim() || !code.value.trim()) {
    notifyError(t('login.required'))
    return
  }

  loading.value = true
  try {
    const result = await api.login(username.value.trim(), code.value.trim())
    setSession(result.user)
    navigate('/')
  } catch (reason) {
    notifyError(t('login.invalid') + ' ' + errorMessage(reason))
  } finally {
    loading.value = false
  }
}

function toggleTheme(): void {
  prefs.theme = prefs.theme === 'dark' ? 'light' : 'dark'
}

function toggleLocale(): void {
  prefs.locale = prefs.locale === 'zh' ? 'en' : 'zh'
}
</script>

<template>
  <div class="login-page">
    <div class="login-glow"></div>

    <div class="login-topbar">
      <span class="login-wordmark">{{ t('login.brand') }}</span>
      <div class="login-topbar-actions">
        <button class="icon-btn" type="button" @click="toggleTheme">
          <AppIcon :name="prefs.theme === 'dark' ? 'sun' : 'moon'" :size="18" />
        </button>
        <button class="lang-btn" type="button" @click="toggleLocale">
          {{ prefs.locale === 'zh' ? 'EN' : '中' }}
        </button>
      </div>
    </div>

    <div class="login-content">
      <section class="login-brand">
        <span class="login-badge">{{ t('login.badge') }}</span>
        <h1>{{ t('app.name') }}</h1>
        <p class="login-tagline">{{ t('app.tagline') }}</p>
        <ul class="login-points">
          <li><AppIcon name="check" :size="15" /> HTTP/3 · gRPC · WebSocket</li>
          <li><AppIcon name="check" :size="15" /> Sites · Certificates · Gateways</li>
          <li><AppIcon name="check" :size="15" /> Access logs · Stats · Cache</li>
        </ul>
      </section>

      <section class="login-card">
        <div class="login-card-head">
          <span class="brand-mark">WG</span>
          <div>
            <h2>{{ t('login.title') }}</h2>
            <p>{{ t('login.subtitle') }}</p>
          </div>
        </div>

        <form class="login-form" @submit.prevent="submit">
          <label class="field-label" for="username">{{ t('login.username') }}</label>
          <input
            id="username"
            v-model="username"
            class="input"
            type="text"
            autocomplete="username"
            :placeholder="t('login.usernamePlaceholder')"
          />

          <span class="label-with-tip">
            <label class="field-label" for="code">{{ t('login.code') }}</label>
            <InfoTip :text="t('login.hint')" />
          </span>
          <input
            id="code"
            v-model="code"
            class="input"
            type="text"
            inputmode="numeric"
            autocomplete="one-time-code"
            maxlength="6"
            :placeholder="t('login.codePlaceholder')"
          />

          <button class="btn primary full" type="submit" :disabled="loading">
            <AppIcon name="arrow" :size="16" />
            {{ loading ? t('login.submitting') : t('login.submit') }}
          </button>
        </form>
      </section>
    </div>
  </div>
</template>
