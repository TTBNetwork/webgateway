<script setup lang="ts">
import NavIcon from './NavIcon.vue'

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
    <div class="stat-top">
      <span class="stat-label">{{ props.label }}</span>
      <span class="stat-icon"><NavIcon :name="props.icon" :size="15" /></span>
    </div>
    <p class="stat-value">{{ props.value }}</p>
    <p v-if="props.hint" class="stat-hint">{{ props.hint }}</p>
  </section>
</template>
