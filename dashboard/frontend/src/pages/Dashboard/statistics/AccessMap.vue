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
import { computed, defineAsyncComponent, onMounted, ref, watch } from 'vue';
import Panel from '../../../components/Panel.vue';
import type { MapType } from '../../../types/access';
import { get_access_map } from '../../../apis/access';
import { debounce } from 'vue-debounce';
const vchart = defineAsyncComponent(() => import('vue-echarts'));
const type = ref<MapType>('global');
const props = defineProps({
    in_days: {
        type: Number,
        default: 1,
    },
});
const data = ref([]);
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
async function refresh() {
    const resp = await get_access_map(props.in_days, type.value);
    console.log(resp);
}
watch(() => type.value, debounce(refresh, 500));
onMounted(async () => {
    await refresh();
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
