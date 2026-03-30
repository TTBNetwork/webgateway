<template>
    <Panel>
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
