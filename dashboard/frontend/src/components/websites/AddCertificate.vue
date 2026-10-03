<template>
    <Dialog>
        <template #header>添加证书</template>
        <template #content>
            <div class="content">
                <InputEdit
                    label="名称"
                    placeholder="仅作为标识使用"
                    v-model:value="name"
                />
                <SelectOptions
                    :data="['自动签发', '手动上传']"
                    v-model:active="active"
                />
                <template v-if="active == 0">
                    <InputEdit
                        label="签发域名"
                        v-model:tags="domains"
                        :muitloptions="true"
                    />
                    <!-- `InputEdit` 的模型名是 `value` / `tags`，没有 `modelValue`：
                         写成 `v-model` 绑不上，字段永远是空的。 -->
                    <InputEdit label="签发证书邮箱" v-model:value="email" />
                </template>
                <template v-if="active == 1">
                    <DragFileIntoInput v-model:value="fullchain"
                        ><InputEdit
                            label="证书公钥"
                            :textarea="true"
                            v-model:value="fullchain"
                    /></DragFileIntoInput>
                    <DragFileIntoInput v-model:value="privkey"
                        ><InputEdit
                            label="证书私钥"
                            :textarea="true"
                            v-model:value="privkey"
                    /></DragFileIntoInput>
                </template>
            </div>
        </template>
        <template #footer
            ><DialogClose @cancel="cancel" type="submit" @confirm="submit"
        /></template>
    </Dialog>
</template>

<script setup lang="ts">
import { ref, watch } from 'vue';
import Dialog from '../../plugins/dialog/Dialog.vue';
import DialogClose from '../../plugins/dialog/DialogClose.vue';
import InputEdit from '../InputEdit.vue';
import { addDialog } from '../../plugins/dialog';
import DraftContent from '../../plugins/dialog/templates/DraftContent.vue';
import SelectOptions from '../SelectOptions.vue';
import DragFileIntoInput from '../DragFileIntoInput.vue';
import type {
    CreateCertificateAuto,
    CreateCertificateManual,
    CreateCertificateType,
} from '../../types/certificate';
import { create } from '../../apis/certificate';
import addPresentation from '../../plugins/presentation';

const emit = defineEmits(['close']);
const name = ref('');
const active = ref(0);
const email = ref('');
const domains = ref<string[]>([]);
const fullchain = ref('');
const privkey = ref('');

const modified = ref(false);
watch(
    () => [
        name.value,
        active.value,
        fullchain.value,
        privkey.value,
        domains.value,
        email.value,
    ],
    () => {
        modified.value = true;
    },
    { deep: true },
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
    const type: CreateCertificateType = active.value == 0 ? 'auto' : 'manual';
    const auto_data: CreateCertificateAuto = {
        dns_provider_id: '',
        hostnames: domains.value,
        email: email.value,
    };
    const manual_data: CreateCertificateManual = {
        fullchain: fullchain.value,
        private_key: privkey.value,
    };
    const resp = await create(
        type,
        type == 'auto' ? auto_data : manual_data,
        name.value ? name.value : undefined,
    );
    if (resp.status == 200) {
        emit('close');
        return;
    }
    addPresentation(resp.message as string, 'alert');
}
</script>

<style lang="css" scoped>
/* 左右内边距由 Dialog 的 `.dialog-content` 统一提供，这里只留分组间距。 */
.content {
    width: 100%;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 16px;
}
</style>
