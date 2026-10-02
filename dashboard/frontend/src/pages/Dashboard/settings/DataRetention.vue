<template>
    <Panel class="retention">
        <div class="title">访问日志保留期</div>
        <div class="desc">
            访问日志表没有分区，会随时间无限增长。这里配置「只保留最近多少天」，
            由数据面（gateway）每小时自动清理一次历史数据。
            <br />
            <strong>下限 90 天（约 3 个月）</strong>；设为 3650 天表示永久保留、不做清理。
        </div>

        <div class="row">
            <InputEdit
                v-model:value="days"
                type="number"
                label="保留天数"
                placeholder="180"
                :disabled="loading"
            />
        </div>

        <div class="row">
            <label class="switch">
                <input
                    type="checkbox"
                    v-model="enabled"
                    :disabled="loading"
                />
                <span>启用自动清理</span>
            </label>
        </div>

        <div class="hint" v-if="Number(days) < MIN_RETENTION_DAYS">
            低于 {{ MIN_RETENTION_DAYS }} 天的设置会被自动调整为
            {{ MIN_RETENTION_DAYS }} 天。
        </div>

        <div class="row buttons">
            <Button :processing="saving" @click="save">保存配置</Button>
            <Button
                reverse-color
                :processing="pruning"
                :disabled="saving"
                @click="pruneNow"
            >
                立即清理一轮
            </Button>
        </div>

        <div class="hint" v-if="message">{{ message }}</div>
    </Panel>
</template>

<script setup lang="ts">
import { onMounted, ref } from 'vue';
import Panel from '../../../components/Panel.vue';
import Button from '../../../components/Button.vue';
import InputEdit from '../../../components/InputEdit.vue';
import { get_retention, prune_now, set_retention } from '../../../apis/settings';
import { MIN_RETENTION_DAYS } from '../../../types/settings';

const days = ref<number>(180);
const enabled = ref<boolean>(true);
const loading = ref(false);
const saving = ref(false);
const pruning = ref(false);
const message = ref('');

async function load() {
    loading.value = true;
    try {
        const cfg = await get_retention();
        days.value = cfg.retention_days;
        enabled.value = cfg.enabled;
    } catch (e) {
        message.value = `读取配置失败：${e instanceof Error ? e.message : e}`;
    } finally {
        loading.value = false;
    }
}

async function save() {
    saving.value = true;
    message.value = '';
    try {
        // 后端会做夹紧，必须用返回值回显，否则界面会显示未生效的数字。
        const cfg = await set_retention(Number(days.value), enabled.value);
        days.value = cfg.retention_days;
        enabled.value = cfg.enabled;
        message.value = `已保存：保留 ${cfg.retention_days} 天，自动清理${
            cfg.enabled ? '已启用' : '已停用'
        }。`;
    } catch (e) {
        message.value = `保存失败：${e instanceof Error ? e.message : e}`;
    } finally {
        saving.value = false;
    }
}

async function pruneNow() {
    pruning.value = true;
    message.value = '';
    try {
        const deleted = await prune_now();
        message.value =
            deleted > 0
                ? `已删除 ${deleted} 行历史日志。可再次点击继续清理。`
                : '没有可清理的历史数据（或另一个实例正在清理）。';
    } catch (e) {
        message.value = `清理失败：${e instanceof Error ? e.message : e}`;
    } finally {
        pruning.value = false;
    }
}

onMounted(load);
</script>

<style scoped>
.retention {
    display: flex;
    flex-direction: column;
    gap: 16px;
    padding: 16px;
}
.title {
    font-weight: bold;
}
.desc {
    font-size: 0.875rem;
    color: var(--text-color);
    opacity: 0.85;
    line-height: 1.6;
}
.row {
    display: flex;
    align-items: center;
    gap: 12px;
}
.buttons {
    gap: 12px;
}
.switch {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    cursor: pointer;
    user-select: none;
}
.hint {
    font-size: 0.875rem;
    color: var(--main-color);
}
</style>
