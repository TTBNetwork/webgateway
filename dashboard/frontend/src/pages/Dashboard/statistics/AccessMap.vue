<template>
    <Panel class="access-map-root">
        <div class="title">
            <div>地理位置</div>
            <div></div>
        </div>
        <div class="value">
            <vchart :option="options" :autoresize="true"></vchart>
        </div>
    </Panel>
</template>
<script setup lang="ts">
import {
    computed,
    defineAsyncComponent,
    onMounted,
    onUnmounted,
    ref,
    watch,
} from 'vue';
import Panel from '../../../components/Panel.vue';
import type { MapType } from '../../../types/access';
import { get_access_map } from '../../../apis/access';
import { isAbortError } from '../../../utils';
const vchart = defineAsyncComponent(() => import('vue-echarts'));
const type = ref<MapType>('global');
const props = defineProps({
    in_days: {
        type: Number,
        default: 1,
    },
});
interface MapSeriesItem {
    name: string;
    value: number;
}
const data = ref<MapSeriesItem[]>([]);
const options = computed(() => ({
    tooltip: {
        trigger: 'item',
    },
    visualMap: {
        min: 0,
        max: 100,
        inRange: {
            color: ['#eefbfb', '#0FC6C2'],
        },
        textStyle: {
            color: '#0FC6C2',
        },
        orient: 'horizontal',
    },
    series: {
        type: 'map',
        map: type.value,
        itemStyle: {
            normal: {
                areaColor: '#F7F8FA', //'#F7F8FA',
                borderColor: '#CCC', //'#CCC'
            },
            emphasis: {
                areaColor: '#ADD8E6', //'#ADD8E6',
                borderColor: '#ffffff', //'#ffffff'
            },
        },
        data: data.value,
    },
}));
const SWITCH_DEBOUNCE_MS = 300;
let controller: AbortController | undefined;
let requestSeq = 0;
let switchTimer: ReturnType<typeof setTimeout> | undefined;

async function refresh(): Promise<void> {
    const seq = ++requestSeq;
    controller?.abort();
    const current = new AbortController();
    controller = current;
    try {
        // 后端返回 HashMap<国家/地区, 次数>，转换为 ECharts map 需要的 { name, value } 序列
        const resp = await get_access_map(
            props.in_days,
            type.value,
            current.signal,
        );
        if (seq !== requestSeq) return; // 过期响应，直接丢弃
        data.value = Object.entries(resp).map(([name, value]) => ({
            name,
            value,
        }));
    } catch (error) {
        // 被新请求取代不算失败；真正的失败保留旧数据
        if (isAbortError(error)) return;
        console.error('获取访问地图失败', error);
    }
}

// 切换时间范围（以及未来的地图类型切换）时防抖刷新，避免叠加全量聚合
watch(
    () => [props.in_days, type.value],
    () => {
        if (switchTimer !== undefined) clearTimeout(switchTimer);
        switchTimer = setTimeout(() => {
            switchTimer = undefined;
            void refresh();
        }, SWITCH_DEBOUNCE_MS);
    },
);

onMounted(async () => {
    await refresh();
});
onUnmounted(() => {
    if (switchTimer !== undefined) clearTimeout(switchTimer);
    controller?.abort();
});
</script>

<style>
.access-map-root.panel {
    min-width: 0px;
    width: 100%;
    height: 384px;
}
.access-map-root .value {
    display: flex;
    width: 100%;
    height: 100%;
    justify-content: center;
    align-items: center;
}
</style>
