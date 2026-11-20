<script setup lang="ts">
import { computed, onMounted, watchEffect } from 'vue'
import AppLayout from './layouts/AppLayout.vue'
import LoginView from './views/LoginView.vue'
import { api } from './api'
import { navigate, route } from './router'
import { clearSession, session, setSession } from './session'

const isLoginRoute = computed(() => route.path === '/login')
const authenticated = computed(() => session.user !== null)
const showLogin = computed(() => !authenticated.value || isLoginRoute.value)

onMounted(async () => {
  try {
    setSession(await api.me())
  } catch {
    clearSession()
  }
})

watchEffect(() => {
  if (!session.ready) return
  if (!authenticated.value && !isLoginRoute.value) {
    navigate('/login')
  } else if (authenticated.value && isLoginRoute.value) {
    navigate('/')
  }
})
</script>

<template>
  <div v-if="!session.ready" class="boot-screen">
    <span class="brand-mark">WG</span>
  </div>
  <LoginView v-else-if="showLogin" />
  <AppLayout v-else />
</template>
