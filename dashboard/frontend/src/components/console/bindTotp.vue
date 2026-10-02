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
                    <QRCodeClient
                        :text="`${bindTotpResponse?.qr_url}`"
                    ></QRCodeClient>
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
import { ref, watch } from 'vue';
import Dialog from '../../plugins/dialog/Dialog.vue';
import DialogClose from '../../plugins/dialog/DialogClose.vue';
import type { BindTotpResponse, BindTotpState } from '../../types/auth';
import InputEdit from '../InputEdit.vue';
import { addDialog } from '../../plugins/dialog';
import DraftContent from '../../plugins/dialog/templates/DraftContent.vue';
import { bindTotp, refreshBindTotpQrcode, verifyBindTotp } from '../../auth';
import addPresentation from '../../plugins/presentation';
import { QRCodeClient } from 'vue3-next-qrcode';
import { useVisiblePolling } from '../../composables/useVisiblePolling';

const emit = defineEmits(['close']);
const state = ref<BindTotpState>('input');
const consoleInput = ref('');
const qrcodeInput = ref('');
const modified = ref(false);
const bindTotpResponse = ref<BindTotpResponse>();

async function refreshBindTotpQrcodeNow(): Promise<void> {
    const resp = await refreshBindTotpQrcode(
        bindTotpResponse.value?.secret_id || '',
    );
    if (resp.status != 200) {
        throw new Error(resp.message || '刷新动态密码二维码失败');
    }
    bindTotpResponse.value = resp.data;
}

// 可见性门控 + 失败退避的轮询器；绑定成功后必须 stop()，否则会一直申请新密钥
const totpQrcodePoller = useVisiblePolling({
    interval: 1000 * 300,
    refresh: refreshBindTotpQrcodeNow,
    onError: (error) => {
        addPresentation(
            error instanceof Error ? error.message : String(error),
            'alert',
        );
    },
});

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
        totpQrcodePoller.start();
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
        // 绑定已成功，停止每 5 分钟申请新 TOTP 密钥的轮询
        totpQrcodePoller.stop();
        state.value = 'verified';
        //state.value = 'verified';
    } else {
        emit('close');
    }
}
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
