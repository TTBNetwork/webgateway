<script setup lang="ts">
import AppIcon from '../components/AppIcon.vue'
import { t } from '../i18n'
import { prefs } from '../prefs'
import { navigate } from '../router'
import { clearSession, session } from '../session'

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
            <dd>{{ session.user ? t('settings.tokenStored') : t('settings.tokenMissing') }}</dd>
          </div>
        </dl>
      </section>
    </div>
  </div>
</template>
