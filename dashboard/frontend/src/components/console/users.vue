<template>
    <Table
        :config="config"
        :data="data"
        @current-page="(v) => (currentPage = v)"
        @page-size="(v) => (perPage = v)"
    >
        <template #header>控制台用户列表</template>
    </Table>
</template>

<script setup lang="ts">
import { computed, onMounted, ref } from 'vue';
import Table from '../../components/Table.vue';
import { formatDate } from '../../utils';
import { getAllUsers } from '../../auth';
import type { UserInfo } from '../../types';
const users = ref<UserInfo[]>();
const config = ref({
    total: 0,
    headers: [
        {
            text: '用户名',
            field: 'username',
        },
        {
            text: '最后操作时间',
            field: 'updated_at',
        },
    ],
});
const data = computed(() => {
    return users.value?.map((v) => ({
        username: v.username,
        updated_at: formatDate(v.updated_at),
    }));
});
const perPage = ref(10);
const currentPage = ref(1);
onMounted(async () => {
    users.value = (await getAllUsers()).data;
});
</script>
