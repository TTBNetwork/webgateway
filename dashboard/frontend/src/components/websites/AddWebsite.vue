<template>
    <Dialog>
        <template #header>{{ isEdit ? '编辑站点' : '添加站点' }}</template>
        <template #content>
            <div class="content">
                <section class="group">
                    <h3 class="group-title">基本信息</h3>
                    <InputEdit
                        label="网站名称"
                        placeholder="可选，便于在面板中识别"
                        v-model:value="state.name"
                    />
                </section>

                <section class="group">
                    <h3 class="group-title">监听规则</h3>
                    <InputEdit
                        label="匹配域名"
                        :muitloptions="true"
                        placeholder="回车添加，支持 * 通配，例如 *.example.com"
                        v-model:tags="state.domains"
                    />
                    <InputEdit
                        label="开放端口"
                        :muitloptions="true"
                        placeholder="回车添加，例如 80 / 443"
                        v-model:tags="state.ports"
                    />
                    <InputEdit
                        label="网站证书"
                        placeholder="回车添加证书 ID；留空则自动选择"
                        :muitloptions="true"
                        v-model:tags="state.cert"
                    />
                </section>

                <section class="group">
                    <div class="group-head">
                        <h3 class="group-title">上游后端</h3>
                        <button type="button" class="add-backend" @click="addBackend">
                            + 添加后端
                        </button>
                    </div>
                    <p v-if="state.backends.length === 0" class="empty-tip">
                        还没有后端，点「+ 添加后端」至少添加一个回源地址。
                    </p>
                    <div
                        v-for="(backend, idx) in state.backends"
                        :key="idx"
                        class="backend-row"
                    >
                        <span class="backend-index">{{ idx + 1 }}</span>
                        <AddWebsiteBackend
                            v-model:url="backend.url"
                            v-model:balance="backend.balance"
                        />
                        <button
                            type="button"
                            class="remove-backend"
                            :disabled="state.backends.length <= 1"
                            :title="
                                state.backends.length <= 1
                                    ? '至少保留一个后端'
                                    : '删除这一行'
                            "
                            @click="removeBackend(idx)"
                        >
                            删除
                        </button>
                    </div>
                </section>
            </div>
        </template>
        <template #footer
            ><DialogClose type="submit" @cancel="cancel" @confirm="submit"
        /></template>
    </Dialog>
</template>

<script setup lang="ts">
import { computed, nextTick, reactive, ref, watch } from 'vue';
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

function addBackend() {
    state.backends.push({ url: '', balance: 0 });
}

function removeBackend(idx: number) {
    if (state.backends.length <= 1) return;
    state.backends.splice(idx, 1);
}

// 编辑模式下预填不算"已修改"，否则每次关掉编辑框都会被追问是否放弃修改。
//
// 判定方式是"与打开时的快照比较"，而不是"任意一次变更就算改过"：
// 后者会被程序性变更（新增/删除一行后端）误触发 —— 用户点了「+ 添加后端」
// 再点「取消」，就会莫名其妙被追问"是否放弃修改"。
const initialSnapshot = ref('');
function currentSnapshot(): string {
    return JSON.stringify({
        name: state.name,
        ports: state.ports,
        domains: state.domains,
        cert: state.cert,
        backends: state.backends,
    });
}
const modified = ref(false);
watch(
    () => currentSnapshot(),
    (now) => {
        modified.value = now !== initialSnapshot.value;
    },
    { deep: true },
);

// 首帧之后再抓快照：`InputEdit` 之类子组件可能在挂载时补写默认值。
nextTick(() => {
    initialSnapshot.value = currentSnapshot();
});

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
/*
 * 内边距交给 Dialog 的 `.dialog-content` 统一提供（左右 24px），
 * 这里只负责分组与间距 —— 以前是 24 + 16 = 40px 的双层内边距，窄屏下很浪费。
 */
.content {
    width: 100%;
    display: flex;
    flex-direction: column;
    gap: 20px;
    padding: 0;
    box-sizing: border-box;
}
.group {
    display: flex;
    flex-direction: column;
    gap: 12px;
}
.group-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
}
.group-title {
    margin: 0;
    font-size: 13px;
    font-weight: 600;
    color: var(--text-color);
    opacity: 0.7;
    letter-spacing: 0.02em;
}
.add-backend,
.remove-backend {
    font: inherit;
    font-size: 13px;
    line-height: 1;
    padding: 6px 10px;
    border-radius: 6px;
    cursor: pointer;
    border: 1px solid rgba(127, 127, 127, 0.4);
    background: transparent;
    color: var(--text-color);
    transition:
        background-color 150ms,
        opacity 150ms;
}
.add-backend:hover,
.remove-backend:not(:disabled):hover {
    background-color: rgba(127, 127, 127, 0.16);
}
.remove-backend:disabled {
    opacity: 0.4;
    cursor: not-allowed;
}
.backend-row {
    display: flex;
    align-items: center;
    gap: 12px;
}
.backend-index {
    flex: 0 0 20px;
    text-align: center;
    font-size: 12px;
    opacity: 0.6;
    font-variant-numeric: tabular-nums;
}
.empty-tip {
    margin: 0;
    font-size: 13px;
    opacity: 0.6;
}
</style>
