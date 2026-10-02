<template>
    <div class="metrics-root">
        <div class="metrics-info">
            <div class="metrics-overview">
                <Panel class="metrics-view data-2">
                    <PanelViewData>
                        <template #title> 请求次数 </template>
                        <template #value>
                            <DataView
                                :data="data?.total_requests || 0"
                                :format="formatNumber"
                            ></DataView>
                        </template>
                    </PanelViewData>
                    <div class="spt-line"></div>
                    <PanelViewData>
                        <template #title> 独立 IP </template>
                        <template #value>
                            <DataView
                                :data="data?.total_ips || 0"
                                :format="formatNumber"
                            ></DataView>
                        </template>
                    </PanelViewData>
                </Panel>
                <Panel class="metrics-view data-2">
                    <PanelViewData>
                        <template #title> 后端错误数 </template>
                        <template #value>
                            <DataView
                                :data="data?.backend_error_requests || 0"
                                :format="formatNumber"
                            ></DataView>
                        </template>
                    </PanelViewData>
                    <div class="spt-line"></div>
                    <PanelViewData>
                        <template #title> 后端错误率 </template>
                        <template #value>
                            {{
                                (
                                    ((data?.total_requests || 0) == 0
                                        ? 0
                                        : (data?.backend_error_requests || 0) /
                                          (data?.total_requests || 0)) * 100
                                ).toFixed(2)
                            }}%
                        </template>
                    </PanelViewData>
                </Panel>
            </div>
            <div class="metrics-overview">
                <Panel class="metrics-view data-2">
                    <PanelViewData>
                        <template #title> 4xx 错误数 </template>
                        <template #value>
                            <DataView
                                :data="data?.e4xx_requests || 0"
                                :format="formatNumber"
                            ></DataView>
                        </template>
                    </PanelViewData>
                    <div class="spt-line"></div>
                    <PanelViewData>
                        <template #title> 4xx 错误率 </template>
                        <template #value>
                            {{
                                (
                                    ((data?.total_requests || 0) == 0
                                        ? 0
                                        : (data?.e4xx_requests || 0) /
                                          (data?.total_requests || 0)) * 100
                                ).toFixed(2)
                            }}%
                        </template>
                    </PanelViewData>
                </Panel>
                <Panel class="metrics-view data-2">
                    <PanelViewData>
                        <template #title> 5xx 错误数 </template>
                        <template #value>
                            <DataView
                                :data="data?.e5xx_requests || 0"
                                :format="formatNumber"
                            ></DataView>
                        </template>
                    </PanelViewData>
                    <div class="spt-line"></div>
                    <PanelViewData>
                        <template #title> 5xx 错误率 </template>
                        <template #value>
                            {{
                                (
                                    ((data?.total_requests || 0) == 0
                                        ? 0
                                        : (data?.e5xx_requests || 0) /
                                          (data?.total_requests || 0)) * 100
                                ).toFixed(2)
                            }}%
                        </template>
                    </PanelViewData>
                </Panel>
            </div>
        </div>
        <div class="metrics-bytes">
            <Panel class="metrics-view data-1">
                <PanelViewData>
                    <template #title> 请求流量 </template>
                    <template #value>
                        <DataView
                            :data="data?.total_request_size || 0"
                            :format="formatBytes"
                        ></DataView>
                    </template>
                </PanelViewData>
            </Panel>
            <Panel class="metrics-view data-1">
                <PanelViewData>
                    <template #title> 响应流量 </template>
                    <template #value>
                        <DataView
                            :data="data?.total_response_size || 0"
                            :format="formatBytes"
                        ></DataView>
                    </template>
                </PanelViewData>
            </Panel>
        </div>
    </div>
</template>

<script setup lang="ts">
import { onMounted, onUnmounted, ref, watch } from 'vue';
import Panel from '../../../components/Panel.vue';
import PanelViewData from '../../../components/PanelViewData.vue';
import { get_access_info } from '../../../apis/access';
import type { AccessInfo } from '../../../types/access';
import { formatBytes, formatNumber } from '../../../units';
import { isAbortError } from '../../../utils';
import DataView from '../../../components/DataView.vue';
import { useVisiblePolling } from '../../../composables/useVisiblePolling';

const POLL_INTERVAL_MS = 60000;
const SWITCH_DEBOUNCE_MS = 250;

const data = ref<AccessInfo>();
const props = defineProps({
    in_days: {
        type: Number,
        default: 1,
    },
});

// 取消 + 请求序号双重保护：切换区间时取消在途请求，且过期响应永远不会覆盖新数据
let controller: AbortController | undefined;
let requestSeq = 0;
let switchTimer: ReturnType<typeof setTimeout> | undefined;

async function refreshInfo(): Promise<void> {
    const seq = ++requestSeq;
    controller?.abort();
    const current = new AbortController();
    controller = current;
    const inDays = props.in_days;
    try {
        const resp = await get_access_info(inDays, current.signal);
        if (seq !== requestSeq) return; // 过期响应，直接丢弃
        data.value = resp;
    } catch (error) {
        // 被新请求取代的旧请求不是失败，不应触发退避
        if (isAbortError(error)) return;
        throw error; // 交给轮询器记录失败并指数退避
    }
}

const poller = useVisiblePolling({
    interval: POLL_INTERVAL_MS,
    refresh: refreshInfo,
});

// 切换区间做短暂防抖，避免快速点击叠加多个 30 天全量聚合
watch(
    () => props.in_days,
    () => {
        if (switchTimer !== undefined) clearTimeout(switchTimer);
        switchTimer = setTimeout(() => {
            switchTimer = undefined;
            void poller.refreshNow();
        }, SWITCH_DEBOUNCE_MS);
    },
);

onMounted(() => {
    poller.start();
});
onUnmounted(() => {
    if (switchTimer !== undefined) clearTimeout(switchTimer);
    controller?.abort();
});
</script>

<style>
:root {
    --metrics-view-splitter-color: rgba(0, 0, 0, 0.2);
}
:root.dark {
    --metrics-view-splitter-color: rgba(255, 255, 255, 0.2);
}
</style>

<style scoped>
.metrics-view {
    display: flex;
    /* width: auto; */
}
.metrics-view > div {
    flex: 8;
}
.metrics-view .spt-line {
    flex: 1;
    border-left: 0.2px solid var(--metrics-view-splitter-color);
}
.panel {
    min-width: 0%;
    width: auto;
}
.panel.data-2 {
    min-width: 260px;
    width: auto;
}
.panel.data-1 {
    min-width: 200px;
    width: auto;
}
.metrics-overview > div {
    flex: 1;
}
/* .metrics-overview > div:last-child {
    flex: 0.5;
} */
.metrics-overview {
    display: flex;
    width: auto;
    min-width: auto;
    gap: 16px;
    flex-wrap: wrap;
    flex-direction: row;
}
.metrics-overview:first-child.metrics-overview {
    margin-bottom: 24px;
}
.metrics-root {
    width: 100%;
    display: flex;
    gap: 16px;
}
.metrics-info {
    width: 100%;
}
.metrics-bytes {
    display: flex;
    width: auto;
    min-width: auto;
    gap: 16px;
    flex-wrap: nowrap;
    flex-direction: column;
}
.metrics-bytes > div {
    flex: 1;
}

@media (max-width: 1020px) {
    .metrics-root {
        flex-wrap: wrap;
    }
    .metrics-bytes {
        width: 100%;
        flex-wrap: wrap;
        flex-direction: row;
    }
}
</style>
