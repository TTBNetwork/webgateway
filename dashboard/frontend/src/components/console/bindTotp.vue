<template>
    <Dialog>
        <template #header>绑定动态密码</template>
        <template #content>
            <div class="content">
                <div v-if="state == 'input'">
                    <InputEdit
                        label="动态密码"
                        placeholder="请输入从后台获取的动态密码"
                        v-model:value="consoleInput"
                    />
                </div>
                <div v-if="state == 'qrcode'" class="qrcode">
                    <QRCodeVue3
                        :value="`${bindTotpResponse?.qr_url}`"
                    ></QRCodeVue3>
                    <InputEdit
                        label="动态密码"
                        placeholder="请输入绑定后的动态密码"
                        v-model:value="qrcodeInput"
                    />
                </div>
                <div v-if="state == 'verified'">
                    <div>
                        大功告成，现在你可以使用由二维码生成的动态密码进行登录了
                    </div>
                </div>
            </div>
        </template>
        <template #footer
            ><DialogClose @cancel="cancel" @confirm="submit"
        /></template>
    </Dialog>
</template>

<script setup lang="ts">
import { onUnmounted, ref, watch } from 'vue';
import Dialog from '../../plugins/dialog/Dialog.vue';
import DialogClose from '../../plugins/dialog/DialogClose.vue';
import type { BindTotpResponse, BindTotpState } from '../../types/auth';
import InputEdit from '../InputEdit.vue';
import { addDialog } from '../../plugins/dialog';
import DraftContent from '../../plugins/dialog/templates/DraftContent.vue';
import { bindTotp, refreshBindTotpQrcode, verifyBindTotp } from '../../auth';
import addPresentation from '../../plugins/presentation';
import QRCodeVue3 from 'qrcode-vue3';

const emit = defineEmits(['close']);
const state = ref<BindTotpState>('input');
const consoleInput = ref('');
const qrcodeInput = ref('');
const modified = ref(false);
const bindTotpResponse = ref<BindTotpResponse>();
const refreshBindTotpQrcodeTask = ref();
watch(
    () => [state.value, consoleInput.value, qrcodeInput.value],
    () => {
        modified.value = true;
    },
);
function cancel() {
    if (modified.value) {
        addDialog(DraftContent, {
            confirm: () => {
                emit('close');
            },
        });
        return;
    }
    emit('close');
}
async function submit() {
    if (state.value == 'input') {
        const resp = await bindTotp(consoleInput.value);
        if (resp.status != 200) {
            addPresentation(resp.message || '', 'alert');
            return;
        }
        bindTotpResponse.value = resp.data;
        refreshBindTotpQrcodeTask.value = setInterval(async () => {
            const resp = await refreshBindTotpQrcode(
                bindTotpResponse.value?.secret_id || '',
            );
            if (resp.status != 200) {
                addPresentation(resp.message || '', 'alert');
                return;
            }
            bindTotpResponse.value = resp.data;
        }, 1000 * 300);
        state.value = 'qrcode';
    } else if (state.value == 'qrcode') {
        if (qrcodeInput.value == '') {
            return;
        }
        const resp = await verifyBindTotp(
            qrcodeInput.value,
            bindTotpResponse.value?.secret_id || '',
        );
        if (resp.status != 200) {
            addPresentation(resp.message || '', 'alert');
            return;
        }
        state.value = 'verified';
        //state.value = 'verified';
    } else {
        emit('close');
    }
}
onUnmounted(() => {
    clearInterval(refreshBindTotpQrcodeTask.value);
});
</script>

<style lang="css" scoped>
.content {
    width: 100%;
    padding: 16px;
    display: flex;
    flex-direction: column;
    gap: 12px;
}
.qrcode {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 24px;
}
</style>
