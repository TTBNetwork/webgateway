<script setup lang="ts">
import AppIcon from '../components/AppIcon.vue'
import { t } from '../i18n'
import { navigate } from '../router'
</script>

<template>
  <div class="page not-found">
    <div class="not-found-code">404</div>
    <h2>{{ t('notFound.title') }}</h2>
    <p class="page-subtitle">{{ t('notFound.message') }}</p>
    <button class="btn primary" type="button" @click="navigate('/')">
      <AppIcon name="arrow" :size="16" />
      {{ t('notFound.back') }}
    </button>
  </div>
</template>
