<script setup lang="ts">
import AppIcon from './AppIcon.vue'
import { dismissToast, toasts } from '../toast'
</script>

<template>
  <Teleport to="body">
    <div class="toast-stack" aria-live="polite" aria-atomic="false">
      <TransitionGroup name="toast">
        <div
          v-for="toast in toasts"
          :key="toast.id"
          class="toast"
          :class="'toast-' + toast.kind"
          role="status"
        >
          <span class="toast-mark"></span>
          <span class="toast-message">{{ toast.message }}</span>
          <button
            class="toast-close"
            type="button"
            aria-label="close"
            @click="dismissToast(toast.id)"
          >
            <AppIcon name="close" :size="14" />
          </button>
        </div>
      </TransitionGroup>
    </div>
  </Teleport>
</template>
