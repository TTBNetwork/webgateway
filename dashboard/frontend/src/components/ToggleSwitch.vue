<script setup lang="ts">
const model = defineModel<boolean>({ required: true })

defineProps<{
  label: string
}>()
</script>

<template>
  <label class="toggle-switch" :class="{ 'is-on': model }">
    <span class="toggle-switch-label">{{ label }}</span>
    <input v-model="model" type="checkbox" />
    <span class="toggle-switch-track" aria-hidden="true">
      <span class="toggle-switch-thumb"></span>
    </span>
  </label>
</template>

<style scoped>
.toggle-switch {
  display: inline-flex;
  align-items: center;
  gap: 10px;
  flex: 0 0 auto;
  cursor: pointer;
  user-select: none;
}

.toggle-switch-label {
  color: var(--text);
  font-size: 12px;
  white-space: nowrap;
}

.toggle-switch input {
  position: absolute;
  width: 1px;
  height: 1px;
  overflow: hidden;
  opacity: 0;
  pointer-events: none;
}

.toggle-switch-track {
  position: relative;
  display: block;
  width: 36px;
  height: 20px;
  border: 1px solid var(--border-strong);
  border-radius: 999px;
  background: var(--panel-soft);
  transition:
    background 0.2s ease,
    border-color 0.2s ease;
}

.toggle-switch-thumb {
  position: absolute;
  top: 2px;
  left: 2px;
  width: 14px;
  height: 14px;
  border-radius: 50%;
  background: var(--muted);
  box-shadow: 0 1px 3px rgba(0, 0, 0, 0.2);
  transition:
    transform 0.22s cubic-bezier(0.22, 1, 0.36, 1),
    background 0.2s ease;
}

.toggle-switch.is-on .toggle-switch-track {
  border-color: var(--accent);
  background: var(--accent);
}

.toggle-switch.is-on .toggle-switch-thumb {
  transform: translateX(16px);
  background: var(--accent-contrast);
}

.toggle-switch input:focus-visible + .toggle-switch-track {
  outline: 2px solid var(--accent);
  outline-offset: 2px;
}
</style>
