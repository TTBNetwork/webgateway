<script setup lang="ts">
import AppIcon from './AppIcon.vue'

const props = withDefaults(defineProps<{ title: string; hint?: string; icon?: string }>(), {
  hint: '',
  icon: 'dot',
})
</script>

<template>
  <div class="empty">
    <div class="empty-icon"><AppIcon :name="props.icon" :size="26" /></div>
    <p class="empty-title">{{ props.title }}</p>
    <p v-if="props.hint" class="empty-hint">{{ props.hint }}</p>
    <slot />
  </div>
</template>
