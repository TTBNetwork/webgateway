<script setup lang="ts">
import { computed, ref } from 'vue'
import AppIcon from '../components/AppIcon.vue'
import CertificatesView from '../views/CertificatesView.vue'
import DashboardView from '../views/DashboardView.vue'
import DnsProvidersView from '../views/DnsProvidersView.vue'
import GatewaysView from '../views/GatewaysView.vue'
import LogsView from '../views/LogsView.vue'
import NotFoundView from '../views/NotFoundView.vue'
import SettingsView from '../views/SettingsView.vue'
import UsersView from '../views/UsersView.vue'
import WebsitesView from '../views/WebsitesView.vue'
import { api } from '../api'
import { t, type MessageKey } from '../i18n'
import { prefs } from '../prefs'
import { navigate, route } from '../router'
import { clearSession, session } from '../session'

interface NavItem {
  path: string
  key: MessageKey
  icon: string
}

interface NavGroup {
  key: MessageKey
  items: NavItem[]
}

const allGroups: NavGroup[] = [
  {
    key: 'nav.groupOverview',
    items: [{ path: '/', key: 'nav.dashboard', icon: 'dashboard' }],
  },
  {
    key: 'nav.groupTraffic',
    items: [
      { path: '/websites', key: 'nav.websites', icon: 'websites' },
      { path: '/certificates', key: 'nav.certificates', icon: 'certificates' },
      { path: '/dns', key: 'nav.dns', icon: 'dns' },
      { path: '/logs', key: 'nav.logs', icon: 'logs' },
    ],
  },
  {
    key: 'nav.groupInfra',
    items: [{ path: '/gateways', key: 'nav.gateways', icon: 'gateways' }],
  },
  {
    key: 'nav.groupSystem',
    items: [
      { path: '/users', key: 'nav.users', icon: 'users' },
      { path: '/settings', key: 'nav.settings', icon: 'settings' },
    ],
  },
]

const views = {
  '/': DashboardView,
  '/websites': WebsitesView,
  '/certificates': CertificatesView,
  '/dns': DnsProvidersView,
  '/gateways': GatewaysView,
  '/logs': LogsView,
  '/users': UsersView,
  '/settings': SettingsView,
}

const menuOpen = ref(false)
const isAdmin = computed(() => ['admin', 'service'].includes(session.user?.role ?? ''))
const groups = computed<NavGroup[]>(() =>
  allGroups
    .map((group) => ({
      ...group,
      items: group.items.filter((item) => item.path !== '/users' || isAdmin.value),
    }))
    .filter((group) => group.items.length > 0),
)
const navigation = computed(() => groups.value.flatMap((group) => group.items))
const current = computed(() => navigation.value.find((item) => item.path === route.path))
const title = computed(() => (current.value ? t(current.value.key) : t('notFound.title')))
const activeView = computed(() => views[route.path as keyof typeof views] ?? NotFoundView)

function go(path: string): void {
  menuOpen.value = false
  navigate(path)
}

function toggleTheme(): void {
  prefs.theme = prefs.theme === 'dark' ? 'light' : 'dark'
}

function toggleLocale(): void {
  prefs.locale = prefs.locale === 'zh' ? 'en' : 'zh'
}

async function logout(): Promise<void> {
  try {
    await api.logout()
  } catch {
    // The session may already be invalid; dropping the local state is enough.
  }
  clearSession()
  navigate('/login')
}
</script>

<template>
  <div class="app-shell">
    <aside class="sidebar" :class="{ 'is-open': menuOpen }">
      <button class="brand" type="button" @click="go('/')">
        <span class="brand-mark">WG</span>
        <span class="brand-text">
          <strong>{{ t('app.name') }}</strong>
          <small>{{ t('app.tagline') }}</small>
        </span>
      </button>

      <nav class="nav" aria-label="Main navigation">
        <div v-for="group in groups" :key="group.key" class="nav-group">
          <p class="nav-group-label">{{ t(group.key) }}</p>
          <button
            v-for="item in group.items"
            :key="item.path"
            type="button"
            class="nav-item"
            :class="{ active: route.path === item.path }"
            :aria-current="route.path === item.path ? 'page' : undefined"
            @click="go(item.path)"
          >
            <AppIcon :name="item.icon" :size="19" />
            <span>{{ t(item.key) }}</span>
          </button>
        </div>
      </nav>

      <div class="sidebar-footer">
        <div class="sidebar-status">
          <span class="status-dot"></span>
          <span>{{ session.user?.username ?? 'service' }}</span>
        </div>
        <small>{{ session.user?.role ?? 'service' }}</small>
      </div>
    </aside>

    <div class="app-main">
      <header class="topbar">
        <button
          class="icon-btn menu-btn"
          type="button"
          :aria-label="t('common.actions')"
          @click="menuOpen = !menuOpen"
        >
          <AppIcon name="menu" :size="20" />
        </button>

        <div class="topbar-title">
          <p class="eyebrow">{{ t('app.name') }}</p>
          <h1>{{ title }}</h1>
        </div>

        <div class="topbar-actions">
          <button class="icon-btn" type="button" :title="t('common.theme')" @click="toggleTheme">
            <AppIcon :name="prefs.theme === 'dark' ? 'sun' : 'moon'" :size="18" />
          </button>
          <button
            class="lang-btn"
            type="button"
            :title="t('common.language')"
            @click="toggleLocale"
          >
            {{ prefs.locale === 'zh' ? 'EN' : '中' }}
          </button>
          <button class="ghost-btn logout-btn" type="button" @click="logout">
            <AppIcon name="logout" :size="16" />
            <span>{{ t('common.logout') }}</span>
          </button>
        </div>
      </header>

      <main class="content">
        <component :is="activeView" />
      </main>

      <footer class="footer">
        <span>{{ t('app.footer') }}</span>
        <span>schema v6 · 3000/api</span>
      </footer>
    </div>

    <div v-if="menuOpen" class="sidebar-backdrop" @click="menuOpen = false"></div>
  </div>
</template>
