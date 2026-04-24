<template>
    <Panel class="access-statistics-root">
        <div class="title">
            <div>访问情况</div>
            <div></div>
        </div>
        <div>
            <vchart></vchart>
        </div>
    </Panel>
</template>
<script setup lang="ts">
import { defineAsyncComponent } from 'vue';
import Panel from '../../../components/Panel.vue';
const vchart = defineAsyncComponent(() => import('vue-echarts'));
</script>

<style>
.access-statistics-root.panel {
    min-width: 322px;
    height: auto; /* 根据布局调整 */
}
</style>
