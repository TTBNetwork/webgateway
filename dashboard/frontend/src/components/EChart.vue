<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref, watch } from 'vue'

interface EChartsInstance {
  setOption(option: Record<string, unknown>, notMerge?: boolean): void
  resize(): void
  dispose(): void
}

interface EChartsStatic {
  init(
    element: HTMLElement,
    theme?: string | null,
    options?: { renderer?: 'canvas' | 'svg' },
  ): EChartsInstance
  getMap(name: string): unknown
}

declare global {
  interface Window {
    echarts?: EChartsStatic
  }
}

const props = withDefaults(defineProps<{ option: Record<string, unknown>; map?: boolean }>(), {
  map: false,
})

const element = ref<HTMLDivElement | null>(null)
let chart: EChartsInstance | null = null
let observer: ResizeObserver | null = null
let mounted = true

let echartsPromise: Promise<EChartsStatic> | null = null
let worldPromise: Promise<void> | null = null

function loadScript(src: string): Promise<void> {
  return new Promise((resolve, reject) => {
    const existing = document.querySelector<HTMLScriptElement>('script[data-wg-src="' + src + '"]')
    if (existing) {
      if (existing.dataset.loaded === 'true') resolve()
      else {
        existing.addEventListener('load', () => resolve(), { once: true })
        existing.addEventListener('error', () => reject(new Error('failed to load ' + src)), {
          once: true,
        })
      }
      return
    }
    const script = document.createElement('script')
    script.src = src
    script.async = true
    script.dataset.wgSrc = src
    script.addEventListener(
      'load',
      () => {
        script.dataset.loaded = 'true'
        resolve()
      },
      { once: true },
    )
    script.addEventListener('error', () => reject(new Error('failed to load ' + src)), {
      once: true,
    })
    document.head.appendChild(script)
  })
}

async function getECharts(): Promise<EChartsStatic> {
  if (window.echarts) return window.echarts
  if (!echartsPromise) {
    echartsPromise = loadScript('/echarts/echarts.min.js').then(() => {
      if (!window.echarts) throw new Error('ECharts did not initialize')
      return window.echarts
    })
  }
  return echartsPromise
}

async function getWorld(echarts: EChartsStatic): Promise<void> {
  if (echarts.getMap('world')) return
  if (!worldPromise) {
    worldPromise = loadScript('/echarts/world.js')
  }
  await worldPromise
}

async function ensureChart(): Promise<void> {
  if (!element.value || chart) return
  const echarts = await getECharts()
  if (props.map) await getWorld(echarts)
  if (!mounted || !element.value) return
  chart = echarts.init(element.value, null, { renderer: 'canvas' })
  chart.setOption(props.option, true)
  observer = new ResizeObserver(() => chart?.resize())
  observer.observe(element.value)
}

onMounted(() => {
  void ensureChart().catch((error) => {
    console.error('ECharts init failed', error)
  })
})

watch(
  () => props.option,
  (option) => {
    if (!chart) {
      void ensureChart().catch((error) => console.error('ECharts init failed', error))
      return
    }
    chart.setOption(option, true)
  },
  { deep: true },
)

onBeforeUnmount(() => {
  mounted = false
  observer?.disconnect()
  chart?.dispose()
  chart = null
})
</script>

<template>
  <div ref="element" class="echart"></div>
</template>

<style scoped>
.echart {
  width: 100%;
  height: 100%;
  min-height: 120px;
}
</style>
