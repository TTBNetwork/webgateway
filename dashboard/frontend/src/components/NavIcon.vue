<script setup lang="ts">
withDefaults(defineProps<{ name: string; size?: number }>(), { size: 18 })

const paths: Record<string, string[]> = {
  dashboard: ['M4 19V13', 'M9.5 19V8', 'M15 19v-4', 'M20.5 19V5'],
  websites: [
    'M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18z',
    'M3.6 9h16.8',
    'M3.6 15h16.8',
    'M12 3c2.4 2.5 3.8 5.6 3.8 9s-1.4 6.5-3.8 9c-2.4-2.5-3.8-5.6-3.8-9S9.6 5.5 12 3z',
  ],
  certificates: ['M12 3 5 6v5c0 4.2 2.8 7.6 7 9 4.2-1.4 7-4.8 7-9V6l-7-3z', 'M9 12l2 2 4-4'],
  gateways: ['M4 5h16v5H4z', 'M4 14h16v5H4z', 'M8 7.5h.01', 'M8 16.5h.01'],
  dns: [
    'M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18z',
    'M3.6 9h16.8',
    'M3.6 15h16.8',
    'M12 3c2.4 2.5 3.8 5.6 3.8 9s-1.4 6.5-3.8 9c-2.4-2.5-3.8-5.6-3.8-9S9.6 5.5 12 3z',
  ],
  logs: ['M5 6h14', 'M5 12h14', 'M5 18h9', 'M18 17l2 2 3-3'],
  users: ['M12 12a4 4 0 1 0 0-8 4 4 0 0 0 0 8z', 'M4 21c0-3.4 3.6-6 8-6s8 2.6 8 6'],
  settings: ['M4 7h16', 'M4 12h16', 'M4 17h16', 'M8 5v4', 'M16 10v4', 'M10 15v4'],
  alert: ['M12 3 2.5 20h19L12 3z', 'M12 9v5', 'M12 17h.01'],
  shield: ['M12 3l7 3v5c0 4.4-2.8 7.8-7 9-4.2-1.2-7-4.6-7-9V6l7-3z', 'M9 12l2 2 4-4'],
  dot: ['M12 10a2 2 0 1 0 0 4 2 2 0 0 0 0-4z'],
}

function shape(name: string): string[] {
  return paths[name] ?? paths.dot ?? []
}
</script>

<template>
  <svg
    :width="size"
    :height="size"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    stroke-width="1.6"
    stroke-linecap="round"
    stroke-linejoin="round"
    aria-hidden="true"
  >
    <path v-for="(path, index) in shape(name)" :key="index" :d="path" />
  </svg>
</template>
