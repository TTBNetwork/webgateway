<template>
    <Dialog>
        <template #header>{{ isEdit ? '编辑站点' : '添加站点' }}</template>
        <template #content>
            <div class="content">
                <InputEdit
                    label="网站名称"
                    placeholder="可选，便于在面板中识别"
                    v-model:value="state.name"
                />
                <InputEdit
                    label="匹配域名"
                    :muitloptions="true"
                    placeholder="支持 * 以匹配网站域名"
                    v-model:tags="config.domains.value"
                />
                <InputEdit
                    label="开放端口"
                    :muitloptions="true"
                    v-model:tags="config.ports.value"
                />
                <InputEdit
                    label="网站证书"
                    placeholder="留空自动选择证书（填证书 ID）"
                    :muitloptions="true"
                    v-model:tags="config.cert.value"
                />
                <div v-for="(backend, idx) in state.backends" :key="idx">
                    <AddWebsiteBackend
                        v-model:url="backend.url"
                        v-model:balance="backend.balance"
                    />
                </div>
            </div>
        </template>
        <template #footer
            ><DialogClose @cancel="cancel" @confirm="submit"
        /></template>
    </Dialog>
</template>

<script setup lang="ts">
import { computed, reactive, ref, toRefs, watch } from 'vue';
import Dialog from '../../plugins/dialog/Dialog.vue';
import DialogClose from '../../plugins/dialog/DialogClose.vue';
import InputEdit from '../InputEdit.vue';
import AddWebsiteBackend from './AddWebsiteBackend.vue';
import { addDialog } from '../../plugins/dialog';
import DraftContent from '../../plugins/dialog/templates/DraftContent.vue';
import { createWebsite, updateWebsite } from '../../apis/websites';
import type {
    Website,
    WebsiteBackendInput,
    WebsiteCreateRequest,
} from '../../types/websites';
import addPresentation from '../../plugins/presentation';

/// 传入 `website` 即为**编辑模式**（表单预填、提交走 update），
/// 不传则为新增模式。同一个组件复用两种形态，避免两份表单逻辑漂移。
const props = defineProps<{ website?: Website }>();
const isEdit = computed(() => !!props.website);
const emit = defineEmits(['close']);

const state = reactive<{
    name: string;
    ports: string[];
    domains: string[];
    cert: string[];
    backends: WebsiteBackendInput[];
}>({
    name: props.website?.name ?? '',
    // 编辑时用站点现值预填；端口在库里是数字，表单组件按字符串处理。
    ports: (props.website?.ports ?? [80, 443]).map((v) => String(v)),
    domains: props.website?.hosts ? [...props.website.hosts] : ['*'],
    cert: props.website?.certificates ? [...props.website.certificates] : [],
    backends:
        props.website?.backends && props.website.backends.length > 0
            ? props.website.backends.map((b) => ({
                  url: b.url,
                  balance: b.balance ?? 0,
              }))
            : [{ url: '', balance: 0 }],
});
const config = toRefs(state);

// 编辑模式下预填不算"已修改"，否则每次关掉编辑框都会被追问是否放弃修改。
const modified = ref(false);
watch(
    () => [state.name, state.ports, state.domains, state.cert, state.backends],
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

function buildRequest(): WebsiteCreateRequest {
    return {
        name: state.name.trim() === '' ? undefined : state.name.trim(),
        ports: state.ports.map((v) => parseInt(v)).filter((v) => !isNaN(v)),
        hosts: state.domains,
        certificates: state.cert,
        backends: state.backends.map((v) => ({
            url: v.url,
            balance: +v.balance,
            main: true,
        })),
        config: props.website?.config,
    };
}

async function submit() {
    if (state.domains.length == 0 && state.ports.length == 0) {
        addPresentation('请至少填写一个域名或端口', 'alert');
        return;
    }
    if (state.backends.every((b) => b.url.trim() === '')) {
        addPresentation('请至少填写一个后端地址', 'alert');
        return;
    }

    const data = buildRequest();
    const resp = props.website
        ? await updateWebsite(props.website.id, data)
        : await createWebsite(data);

    if (resp.status == 200) {
        addPresentation(isEdit.value ? '修改成功' : '添加成功', 'success');
        emit('close');
    } else {
        addPresentation(resp.message as string, 'alert');
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
</style>
