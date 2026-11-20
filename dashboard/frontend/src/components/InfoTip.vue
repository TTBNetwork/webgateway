<script setup lang="ts">
withDefaults(defineProps<{ text: string; label?: string }>(), { label: 'info' })
</script>

<template>
  <span class="info-tip" tabindex="0" role="note" :aria-label="text">
    <span class="info-tip-icon" aria-hidden="true">?</span>
    <span class="info-tip-bubble" role="tooltip">{{ text }}</span>
  </span>
</template>
