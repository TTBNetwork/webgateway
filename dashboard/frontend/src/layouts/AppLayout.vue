<script setup lang="ts">
import { computed, ref, watch } from 'vue'
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
}

interface NavGroup {
  key: MessageKey
  icon: string
  items: NavItem[]
}

const menuGroups: NavGroup[] = [
  {
    key: 'nav.groupOverview',
    icon: 'dashboard',
    items: [{ path: '/', key: 'nav.dashboard' }],
  },
  {
    key: 'nav.groupTraffic',
    icon: 'websites',
    items: [
      { path: '/websites', key: 'nav.websites' },
      { path: '/certificates', key: 'nav.certificates' },
      { path: '/dns', key: 'nav.dns' },
      { path: '/logs', key: 'nav.logs' },
    ],
  },
  {
    key: 'nav.groupInfra',
    icon: 'gateways',
    items: [{ path: '/gateways', key: 'nav.gateways' }],
  },
  {
    key: 'nav.groupSystem',
    icon: 'settings',
    items: [
      { path: '/users', key: 'nav.users' },
      { path: '/settings', key: 'nav.settings' },
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
  menuGroups
    .map((group) => ({
      ...group,
      items: group.items.filter((item) => item.path !== '/users' || isAdmin.value),
    }))
    .filter((group) => group.items.length > 0),
)
const activeView = computed(() => views[route.path as keyof typeof views] ?? NotFoundView)
const activeGroup = computed(
  () => groups.value.find((group) => group.items.some((item) => item.path === route.path))?.key,
)
const openMenu = ref<MessageKey | null>(null)

watch(
  activeGroup,
  (key) => {
    if (key && groups.value.find((group) => group.key === key)?.items.length === 1) {
      openMenu.value = null
    } else if (key) {
      openMenu.value = key
    }
  },
  { immediate: true },
)

function groupForPath(path: string): NavGroup | undefined {
  return groups.value.find((group) => group.items.some((item) => item.path === path))
}

function firstItem(group: NavGroup): NavItem {
  return group.items[0] as NavItem
}

function toggleGroup(key: MessageKey): void {
  const group = groups.value.find((candidate) => candidate.key === key)
  if (!group) return
  if (openMenu.value === key) {
    openMenu.value = null
    return
  }
  openMenu.value = key
  // Opening a section from outside it should also activate its first page;
  // otherwise the user sees an open menu with no second-level item selected.
  if (!group.items.some((item) => item.path === route.path)) {
    const first = group.items[0]
    if (first) go(first.path)
  }
}

function go(path: string): void {
  menuOpen.value = false
  const group = groupForPath(path)
  if (group && group.items.length > 1) openMenu.value = group.key
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
        <div v-for="group in groups" :key="group.key" class="nav-block">
          <button
            v-if="group.items.length === 1"
            type="button"
            class="nav-item"
            :class="{ active: route.path === firstItem(group).path }"
            :aria-current="route.path === firstItem(group).path ? 'page' : undefined"
            @click="go(firstItem(group).path)"
          >
            <AppIcon :name="group.icon" :size="19" />
            <span>{{ t(group.key) }}</span>
          </button>

          <template v-else>
            <button
              type="button"
              class="nav-item nav-parent"
              :class="{ active: group.items.some((item) => item.path === route.path) }"
              :aria-expanded="openMenu === group.key"
              @click="toggleGroup(group.key)"
            >
              <AppIcon :name="group.icon" :size="19" />
              <span>{{ t(group.key) }}</span>
              <AppIcon class="nav-caret" name="chevron" :size="14" />
            </button>
            <div class="nav-sub-wrap" :class="{ open: openMenu === group.key }">
              <div class="nav-sub-clip">
                <div class="nav-sub">
                  <button
                    v-for="item in group.items"
                    :key="item.path"
                    type="button"
                    class="nav-subitem"
                    :class="{ active: route.path === item.path }"
                    :aria-current="route.path === item.path ? 'page' : undefined"
                    @click="go(item.path)"
                  >
                    <span>{{ t(item.key) }}</span>
                  </button>
                </div>
              </div>
            </div>
          </template>
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
          <h1>{{ activeGroup ? t(activeGroup) : t('app.name') }}</h1>
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
