<script setup lang="ts">
import { useRoute } from 'vue-router';
import Panel from '../../components/Panel.vue';
import Switchbar from '../../components/Switchbar.vue';
import { pushQuery } from '../../constant';
import { computed } from 'vue';
import Metrics from './statistics/metrics.vue';
import QPS from './statistics/QPS.vue';
// import { get_access_map } from '../../apis/access';
// import AccessMap from './statistics/AccessMap.vue';
// import AccessStatistics from './statistics/AccessStatistics.vue';
const options = [
    {
        text: '近 24 小时',
        key: '1',
    },
    {
        text: '近 7 天',
        key: '7',
    },
    {
        text: '近 30 天',
        key: '30',
    },
];
const query = computed(() => useRoute().query);
// get_access_map(1, 'china');
</script>

<template>
    <Panel style="padding: 6px; margin-bottom: 24px">
        <Switchbar
            :data="options"
            @active="(v) => pushQuery({ in_days: v })"
            :active="options.findIndex((v) => query?.in_days == v.key)"
        />
    </Panel>
    <div class="statistics-overview">
        <Metrics :in_days="+(query?.in_days || 1)" />
        <QPS />
    </div>
    <!-- <div class="statistics-overview">
        <AccessMap></AccessMap>
        <AccessStatistics></AccessStatistics>
    </div> -->
</template>

<style>
.statistics-overview {
    display: flex;
    gap: 24px;
    flex-wrap: nowrap;
    margin-bottom: 24px;
}
@media (max-width: 1360px) {
    .statistics-overview {
        flex-wrap: wrap;
    }
}
</style>
