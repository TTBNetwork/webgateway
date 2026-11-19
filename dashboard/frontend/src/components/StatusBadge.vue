<script setup lang="ts">
import { computed } from 'vue'

const props = defineProps<{
  status: number | string | null | undefined
  label?: string
}>()

type Tone = 'success' | 'warning' | 'danger' | 'info' | 'muted'

const tone = computed<Tone>(() => {
  if (typeof props.status === 'number') {
    if (props.status >= 500) return 'danger'
    if (props.status >= 400) return 'warning'
    if (props.status >= 300) return 'info'
    if (props.status >= 200) return 'success'
    return 'muted'
  }
  const value = String(props.status ?? '').toLowerCase()
  if (['online', 'valid', 'ok', 'success'].includes(value)) return 'success'
  if (['expiring', 'warning', 'degraded'].includes(value)) return 'warning'
  if (['offline', 'expired', 'error', 'danger'].includes(value)) return 'danger'
  if (['info'].includes(value)) return 'info'
  return 'muted'
})

const text = computed(
  () =>
    props.label ??
    (props.status === null || props.status === undefined ? '—' : String(props.status)),
)
</script>

<template>
  <span class="badge" :class="'badge-' + tone">{{ text }}</span>
</template>
