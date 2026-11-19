<script setup lang="ts">
import AppIcon from './AppIcon.vue'

const props = withDefaults(
  defineProps<{
    label: string
    value: string
    hint?: string
    icon?: string
    tone?: 'default' | 'accent' | 'success' | 'warning' | 'danger'
  }>(),
  { hint: '', icon: 'dot', tone: 'default' },
)
</script>

<template>
  <section class="stat-card" :class="'tone-' + props.tone">
    <div class="stat-icon">
      <AppIcon :name="props.icon" :size="20" />
    </div>
    <div class="stat-body">
      <p class="stat-label">{{ props.label }}</p>
      <p class="stat-value">{{ props.value }}</p>
      <p v-if="props.hint" class="stat-hint">{{ props.hint }}</p>
    </div>
  </section>
</template>
