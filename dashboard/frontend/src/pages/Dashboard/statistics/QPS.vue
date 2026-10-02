<template>
    <Panel class="qps-root">
        <div class="title">
            <span>实时 QPS</span>
            <span>{{ data[data.length - 1]?.count || 0 }}</span>
        </div>
        <div class="value" ref="qps_chart">
            <vchart
                :option="option"
                style="width: 100%; height: 100%"
                :autoresize="true"
            />
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
import { get_qps } from '../../../apis/access';
import { type ResponseQPS, type QPS } from '../../../types/access';
import { isDark } from '../../../theme';
import { darkMainColor, lightMainColor } from '../../../constant';
import { debounce } from 'vue-debounce';
import { useVisiblePolling } from '../../../composables/useVisiblePolling';

// 异步加载 ECharts 组件
const vchart = defineAsyncComponent(() => import('vue-echarts'));

// 响应式数据：存储已补全的 QPS 时间序列
const data = ref<QPS[]>([]);
const respData = ref<ResponseQPS>();

// ECharts 配置项（基于 data 计算）
const option = computed(() => ({
    chartTheme: isDark.value ? 'dark' : 'light',
    color: isDark.value ? darkMainColor : lightMainColor,
    tooltip: { trigger: 'axis' },
    stateAnimation: {
        duration: 300,
        easing: 'cubicOut' as const,
    },
    grid: {
        top: 10,
        bottom: 10,
        right: 0,
        left: 0,
        show: false,
        z: 0,
        containLabel: false,
        backgroundColor: 'rgba(0,0,0,0)',
        borderWidth: 1,
        borderColor: '#ccc',
    },
    xAxis: {
        type: 'category',
        show: false,
        // 将 Date 对象转换为本地时间字符串作为轴标签（虽然隐藏，但 tooltip 会用）
        data: data.value.map((v) => v.time.toLocaleTimeString()),
        splitLine: {
            color: '#FFFFFF',
            type: 'dashed',
        },
    },
    yAxis: {
        show: false,
        type: 'value',
        splitLine: {
            color: '#FFFFFF',
            type: 'dashed',
        },
    },
    series: [
        {
            type: 'bar',
            name: 'QPS',
            data: data.value.map((v) => v.count),
            barGap: '0',
            barMinHeight: 4,
            itemStyle: {
                borderRadius: [2, 2, 0, 0],
            },
            z: 2,
            backgroundStyle: {
                color: 'rgba(180, 180, 180, 0.2)',
                borderColor: null,
                borderWidth: 0,
                borderRadius: 0,
                shadowBlur: 0,
            },
            select: {
                itemStyle: {
                    borderColor: '#212121',
                },
            },
        },
    ],
}));

// 定时器句柄由 useVisiblePolling 内部管理

// 组件属性：显示的数据点数量（默认 60 个，对应 5 分钟）
const qps_chart = ref<HTMLDivElement>();
const count = ref(calcCount());

const observer = new ResizeObserver(() => {
    count.value = calcCount();
    // setData();
});
const INTERVAL_MS = 5000;
const OFFSET_MS = 2500;
function calcCount() {
    return Math.min(
        Math.floor(
            ((qps_chart.value?.getBoundingClientRect().width || 0) / 7.5 ||
                60) / 5,
        ) * 5,
        60,
    );
}

watch(() => [respData.value, count.value], debounce(setData, 500));

function setData() {
    const resp = respData.value;
    if (!resp) return;
    const currentTime = new Date(
        Math.floor(+new Date(resp.current_time) / 5000) * 5000 - 5000,
    );
    const qps: QPS[] = [];
    const mapping: Map<number, number> = new Map();
    resp.data.forEach((item) => {
        mapping.set(+new Date(item.time), item.count);
    });
    for (let i = 0; i < count.value; i++) {
        const t = new Date(currentTime.getTime() - i * INTERVAL_MS);
        qps.push({
            time: t,
            count: mapping.get(+t) || 0,
        });
    }
    data.value = qps.reverse();
}

async function refreshQPS() {
    const resp = await get_qps(undefined);
    // ky 关闭了自动重试且 throwHttpErrors: false，失败时响应仍会 resolve，
    // 因此这里必须显式抛出，交给轮询器做指数退避。
    if (resp.status !== 200 || !resp.data) {
        throw new Error(resp.message || `获取 QPS 数据失败 (${resp.status})`);
    }
    respData.value = resp.data;
    setData();
}

// 对齐到下一个“中间时刻”（与组件初始化逻辑一致）
function nextBoundaryDelay(): number {
    const now = Date.now();
    const nextBoundary =
        Math.ceil((now - OFFSET_MS) / INTERVAL_MS) * INTERVAL_MS + OFFSET_MS;
    return Math.max(0, nextBoundary - now);
}

// 可见性门控 + 失败退避的轮询器；成功时仍按 5s 墙钟边界对齐。
const poller = useVisiblePolling({
    interval: INTERVAL_MS,
    refresh: refreshQPS,
    nextDelay: nextBoundaryDelay,
    maxBackoffMs: 60000,
});

// 组件挂载后立即开始刷新
onMounted(() => {
    poller.start();
    observer.observe(qps_chart.value!);
});

// 组件卸载时停止轮询并断开 ResizeObserver
// （useVisiblePolling 会在卸载时自行清理定时器与 visibilitychange 监听）
onUnmounted(() => {
    observer.disconnect();
});
</script>

<style scoped>
/* 确保 Panel 有足够的高度，否则图表可能不显示 */
.panel.qps-root {
    min-width: 322px;
    /* width: 100%; */
    height: auto; /* 根据布局调整 */
}
.qps-root .title {
    font-size: 16px;
    font-weight: 500;
    display: flex;
    align-items: center;
    flex-wrap: nowrap;
    gap: 8px;
}
.qps-root .value {
    margin-top: 10px;
    height: calc(100% - 34px);
}
@media (max-width: 1360px) {
    .panel.qps-root {
        width: 100%;
    }
}
</style>
