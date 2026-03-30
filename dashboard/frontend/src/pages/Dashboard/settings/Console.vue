<template>
    <Panel class="info">
        <div class="title">控制台用户管理</div>
        <div class="value">
            <div>
                用户名：<span>{{ userInfo?.username }}</span>
            </div>
            <div>
                是否绑定动态密码：<span>{{
                    userInfo?.bound_totp ? '是' : '否'
                }}</span>
            </div>
            <Button @click="addDialog(BindTotp)">绑定动态密码</Button>
        </div>
    </Panel>
    <Users />
</template>

<script setup lang="ts">
import { onMounted, ref } from 'vue';
import Panel from '../../../components/Panel.vue';
import { info } from '../../../auth';
import type { UserInfo } from '../../../types';
import Users from '../../../components/console/users.vue';
import Button from '../../../components/Button.vue';
import { addDialog } from '../../../plugins/dialog';
import BindTotp from '../../../components/console/bindTotp.vue';
const userInfo = ref<UserInfo>();
onMounted(async () => {
    userInfo.value = await info();
});
</script>

<style scoped>
.info {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    justify-content: center;
    gap: 16px;
    margin-bottom: 24px;
}
.title {
    font-weight: bold;
}

.value {
    font-size: 0.975rem;
}

.value span {
    font-weight: bold;
}
</style>
