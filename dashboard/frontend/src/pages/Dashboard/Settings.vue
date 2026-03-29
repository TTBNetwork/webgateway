<script setup lang="ts">
import { useRoute } from 'vue-router';
import Panel from '../../components/Panel.vue';
import Switchbar from '../../components/Switchbar.vue';
import { pushQuery } from '../../constant';
import { computed } from 'vue';
import Log from './settings/Log.vue';
import Console from './settings/Console.vue';
const options = [
    {
        text: '控制台管理',
        key: 'console',
    },
    {
        text: '系统日志',
        key: 'log',
    },
];
const query = computed(() => useRoute().query);
</script>

<template>
    <Panel style="padding: 6px; margin-bottom: 24px">
        <Switchbar
            :data="options"
            @active="(v) => pushQuery({ tab: v })"
            :active="options.findIndex((v) => query?.tab == v.key)"
        />
    </Panel>
    <div v-if="query?.tab == 'log'">
        <Log></Log>
    </div>
    <div v-if="query?.tab == 'console'">
        <Console />
    </div>
</template>
