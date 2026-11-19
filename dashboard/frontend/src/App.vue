<script setup lang="ts">
import { computed, watchEffect } from 'vue'
import AppLayout from './layouts/AppLayout.vue'
import LoginView from './views/LoginView.vue'
import { navigate, route } from './router'
import { session } from './session'

const isLoginRoute = computed(() => route.path === '/login')
const authenticated = computed(() => session.token.length > 0)
const showLogin = computed(() => !authenticated.value || isLoginRoute.value)

watchEffect(() => {
  if (!authenticated.value && !isLoginRoute.value) {
    navigate('/login')
  } else if (authenticated.value && isLoginRoute.value) {
    navigate('/')
  }
})
</script>

<template>
  <LoginView v-if="showLogin" />
  <AppLayout v-else />
</template>
