<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue'
import AppIcon from './AppIcon.vue'

export interface SelectOption {
  value: string | number
  label: string
  hint?: string
}

const props = withDefaults(
  defineProps<{
    modelValue: string | number
    options: SelectOption[]
    placeholder?: string
    disabled?: boolean
  }>(),
  { placeholder: '—', disabled: false },
)
const emit = defineEmits<{ 'update:modelValue': [string | number] }>()

const open = ref(false)
const root = ref<HTMLElement | null>(null)
const selected = computed(() => props.options.find((option) => option.value === props.modelValue))

function choose(value: string | number): void {
  emit('update:modelValue', value)
  open.value = false
}

function onDocumentClick(event: MouseEvent): void {
  if (root.value && !root.value.contains(event.target as Node)) {
    open.value = false
  }
}

onMounted(() => document.addEventListener('click', onDocumentClick))
onUnmounted(() => document.removeEventListener('click', onDocumentClick))
</script>

<template>
  <div ref="root" class="select-field" :class="{ open, disabled }">
    <button
      type="button"
      class="select-trigger"
      :disabled="disabled"
      :aria-expanded="open"
      aria-haspopup="listbox"
      @click="open = !open"
    >
      <span class="select-value">{{ selected ? selected.label : placeholder }}</span>
      <AppIcon name="arrow" :size="14" />
    </button>
    <ul v-if="open" class="select-menu" role="listbox">
      <li v-for="option in options" :key="option.value">
        <button
          type="button"
          class="select-option"
          role="option"
          :aria-selected="option.value === modelValue"
          @click="choose(option.value)"
        >
          <span>{{ option.label }}</span>
          <small v-if="option.hint">{{ option.hint }}</small>
          <AppIcon v-if="option.value === modelValue" name="check" :size="14" />
        </button>
      </li>
    </ul>
  </div>
</template>
