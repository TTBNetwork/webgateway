<template>
    <Panel style="margin-bottom: 24px"
        ><div class="websites-overview">
            <div class="overview-side">
                <span class="count">共 {{ websites.length }} 个网站</span>
                <InputEdit
                    class="search-input"
                    label="网站"
                    placeholder="支持网站名称、域名、后端地址等搜索"
                    v-model:value="keyword"
                ></InputEdit>
            </div>
            <div>
                <Button type="button" @click="toggleAddWebsite"
                    >添加网站</Button
                >
            </div>
        </div></Panel
    >
    <div class="websites" v-if="filtered.length > 0">
        <Panel class="site" v-for="site in filtered" :key="site.id">
            <div class="site-overview">
                <div>
                    <SvgIcon
                        name="common-earth"
                        class="site-default-icon"
                        size="default"
                    ></SvgIcon>
                </div>
                <div class="site-view">
                    <PanelViewData class="small">
                        <template #title>今日请求</template>
                        <template #value
                            ><DataView
                                :data="metrics[site.id]?.total_requests || 0"
                                :format="formatNumber"
                        /></template>
                    </PanelViewData>
                    <div class="spt-line">
                        <div class="spt-line-inner"></div>
                    </div>
                    <PanelViewData class="small">
                        <template #title>今日流量</template>
                        <template #value
                            ><DataView
                                :data="
                                    (metrics[site.id]?.total_request_size ||
                                        0) +
                                    (metrics[site.id]?.total_response_size || 0)
                                "
                                :format="formatBytes"
                        /></template>
                    </PanelViewData>
                </div>
            </div>
            <div class="spt-line">
                <div class="spt-line-inner"></div>
            </div>
            <div class="site-content">
                <div class="site-name">{{ site.name || '无标题' }}</div>
                <div class="site-hosts" :title="site.hosts.join(', ')">
                    {{ site.hosts.join(', ') || '未配置域名' }}
                </div>
                <div class="site-meta">
                    端口 {{ site.ports.join('/') }} · 后端
                    {{ site.backends.length }} 个
                </div>
            </div>
            <div class="site-actions">
                <Button
                    class="site-action"
                    reverse-color
                    @click="toggleEditWebsite(site)"
                    >编辑</Button
                >
                <Button
                    class="site-action"
                    reverse-color
                    @click="confirmDelete(site)"
                    >删除</Button
                >
            </div>
        </Panel>
    </div>
    <Panel v-else>
        <div class="empty">
            {{ keyword ? '没有匹配的网站' : '还没有网站，点右上角「添加网站」开始' }}
        </div>
    </Panel>
</template>

<script lang="ts" setup>
import { computed, onMounted, ref } from 'vue';
import Button from '../../components/Button.vue';
import InputEdit from '../../components/InputEdit.vue';
import Panel from '../../components/Panel.vue';
import type { Website } from '../../types/websites';
import { deleteWebsite, getWebsites } from '../../apis/websites';
import { addDialog, listen, unlisten } from '../../plugins/dialog';
import AddWebsite from '../../components/websites/AddWebsite.vue';
import DraftContent from '../../plugins/dialog/templates/DraftContent.vue';
import SvgIcon from '../../components/SvgIcon.vue';
import PanelViewData from '../../components/PanelViewData.vue';
import type { TodayMetricsInfoOfWebsites } from '../../types/access';
import { get_today_metrics_info_of_websites } from '../../apis/access';
import DataView from '../../components/DataView.vue';
import { formatBytes, formatNumber } from '../../units';
import addPresentation from '../../plugins/presentation';

const websites = ref<Website[]>([]);
const metrics = ref<Record<string, TodayMetricsInfoOfWebsites>>({});
const keyword = ref('');

const filtered = computed(() => {
    const kw = keyword.value.trim().toLowerCase();
    if (!kw) return websites.value;
    return websites.value.filter((site) => {
        const haystack = [
            site.name ?? '',
            ...(site.hosts ?? []),
            ...(site.ports ?? []).map(String),
            ...(site.backends ?? []).map((b) => b.url),
        ]
            .join(' ')
            .toLowerCase();
        return haystack.includes(kw);
    });
});

async function refresh() {
    websites.value = await getWebsites();
    const web_metrics: Record<string, TodayMetricsInfoOfWebsites> = {};
    for (const site of await get_today_metrics_info_of_websites()) {
        if (!site.website_id) continue;
        web_metrics[site.website_id] = site;
    }
    metrics.value = web_metrics;
}

onMounted(refresh);

/// 打开新增/编辑对话框，并在其关闭后刷新列表。
/// 两种模式复用同一个组件（传 `website` 即编辑），避免两份表单逻辑漂移。
function openWebsiteDialog(website?: Website) {
    const id = addDialog(AddWebsite, website ? { website } : undefined);
    const closeId = listen(
        'close',
        async () => {
            await refresh();
            unlisten(closeId);
        },
        id,
    );
}

function toggleAddWebsite() {
    openWebsiteDialog();
}

function toggleEditWebsite(site: Website) {
    openWebsiteDialog(site);
}

/// 删除是破坏性操作（域名立刻停止服务），因此先弹确认框。
function confirmDelete(site: Website) {
    const label = site.name || site.hosts.join(', ') || site.id;
    addDialog(
        DraftContent,
        {
            confirm: async () => {
                const resp = await deleteWebsite(site.id);
                if (resp.status == 200) {
                    addPresentation('删除成功', 'success');
                    await refresh();
                } else {
                    addPresentation(resp.message as string, 'alert');
                }
            },
        },
        { preventConfirm: false },
    );
    addPresentation(`确认删除站点「${label}」？删除后其域名将立即停止服务`, 'alert');
}
</script>

<style>
:root {
    --site-spt-line: rgba(0, 0, 0, 0.3);
}
:root.dark {
    --site-spt-line: rgba(255, 255, 255, 0.3);
}
</style>

<style scoped>
.websites-overview {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 1rem;
    min-height: 56px;
}

.overview-side {
    display: flex;
    align-items: center;
    gap: 1rem;
    flex: 1 1 auto;
}

.search-input {
    width: 288px;
    max-width: 100%;
}

.count {
    white-space: nowrap;
}

/* 每行三个：卡片固定占 1/3 宽（扣掉两道 16px 间距），不足一行时保持同样宽度左对齐。
   窄屏退化为两列、再退化为单列。 */
.websites {
    display: flex;
    flex-wrap: wrap;
    gap: 16px;
    min-height: calc(100% - 93px);
}

.site {
    display: flex;
    flex-direction: column;
    gap: 12px;
    flex: 0 0 calc((100% - 32px) / 3);
    max-width: calc((100% - 32px) / 3);
    box-sizing: border-box;
}

.site-overview {
    display: flex;
    align-items: center;
    gap: 16px;
}

.site-default-icon {
    width: 32px;
    height: 32px;
    border-radius: 50%;
    fill: var(--main-color);
    flex-shrink: 0;
}

.spt-line {
    display: block;
    width: 100%;
    height: 1px;
}

.spt-line-inner {
    display: block;
    width: 100%;
    border-top: 0.5px solid var(--site-spt-line);
}

.site-view {
    display: flex;
    align-items: center;
    gap: 16px;
    flex: 1 1 auto;
}

.site-content {
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-height: 62px;
}

.site-name {
    font-weight: bold;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
}

.site-hosts {
    font-size: 0.875rem;
    opacity: 0.85;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
}

.site-meta {
    font-size: 0.75rem;
    opacity: 0.6;
}

.site-actions {
    display: flex;
    gap: 8px;
}

.site-action {
    flex: 1 1 0;
}

.empty {
    padding: 24px;
    text-align: center;
    opacity: 0.7;
}

@media (max-width: 1100px) {
    .site {
        flex: 0 0 calc((100% - 16px) / 2);
        max-width: calc((100% - 16px) / 2);
    }
}

@media (max-width: 800px) {
    .overview-side {
        flex-wrap: wrap;
        width: 100%;
    }
    .search-input {
        width: 100%;
        max-width: 100%;
    }
    .websites-overview > :last-child {
        margin-left: 0;
        width: 100%;
    }
    .websites-overview > :last-child button {
        width: 100%;
    }
    .site {
        flex: 0 0 100%;
        max-width: 100%;
    }
}
</style>
