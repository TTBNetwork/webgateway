<template>
    <Dialog>
        <template #header>添加解析</template>
        <template #content>
            <div class="content">
                <InputEdit
                    label="名称"
                    placeholder="仅作为标识使用"
                    v-model:value="name"
                />
                <InputEdit
                    label="域名"
                    :muitloptions="true"
                    v-model:tags="domains"
                />
                <InputEdit label="DNS 服务商" :disabled="true" value="腾讯云" />
                <Tencent
                    v-if="type == 'tencent'"
                    :result="tencent_result"
                ></Tencent>
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
import Tencent from './dnsproviders/Tencent.vue';
import { create } from '../../apis/dnsproviders';
import addPresentation from '../../plugins/presentation';
import type { DNSProviderType } from '../../types/dnsproviders';

const emit = defineEmits(['close']);
const name = ref('');
const type = ref<DNSProviderType>('tencent');
const tencent_result = ref({});
const domains = ref([]);

const modified = ref(false);
watch(
    () => [name.value, type.value, tencent_result.value],
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
function get_config() {
    if (type.value == 'tencent') {
        return tencent_result.value;
    }
    return {};
}
async function submit() {
    const resp = await create(
        name.value,
        domains.value,
        type.value,
        get_config(),
    );
    if (resp.status == 200) {
        addPresentation('添加成功', 'success');
        emit('close');
    } else {
        addPresentation(resp.message as string, 'alert');
    }
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
