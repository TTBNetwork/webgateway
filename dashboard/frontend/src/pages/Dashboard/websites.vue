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
            <!-- 第一行：状态标签在左、图标（编辑入口）在右 —— 对齐截图里
                 「防护模式 + 地球图标(编辑)」的信息层级。 -->
            <div class="site-top">
                <span class="site-status">已启用</span>
                <button
                    type="button"
                    class="site-edit-icon"
                    title="编辑站点"
                    aria-label="编辑站点"
                    @click="toggleEditWebsite(site)"
                >
                    <SvgIcon name="common-earth" class="site-icon"></SvgIcon>
                </button>
            </div>

            <!-- 第二行：标题 + 「详情」（同样打开编辑对话框）。 -->
            <div class="site-titlebar">
                <span class="site-name" :title="site.name || undefined">
                    {{ site.name || '无标题' }}
                </span>
                <button
                    type="button"
                    class="site-detail"
                    @click="toggleEditWebsite(site)"
                >
                    详情
                </button>
            </div>

            <!-- 第三行：域名 / 端口，标签与值左对齐成两列。 -->
            <dl class="site-fields">
                <dt>域名</dt>
                <dd :title="site.hosts.join(', ')">
                    {{ site.hosts.join(', ') || '通配所有域名' }}
                </dd>
                <dt>端口</dt>
                <dd>{{ portsText(site) }}</dd>
            </dl>

            <!-- 第四行：两个指标并排，中间一条竖线分隔。 -->
            <div class="site-stats">
                <div class="stat">
                    <div class="stat-label">今日请求</div>
                    <div class="stat-value">
                        <DataView
                            :data="metrics[site.id]?.total_requests || 0"
                            :format="formatNumber"
                        />
                    </div>
                </div>
                <div class="stat">
                    <div class="stat-label">今日流量</div>
                    <div class="stat-value">
                        <DataView
                            :data="
                                (metrics[site.id]?.total_request_size || 0) +
                                (metrics[site.id]?.total_response_size || 0)
                            "
                            :format="formatBytes"
                        />
                    </div>
                </div>
            </div>

            <div class="site-upstream" :title="upstreamText(site)">
                <span class="label">上游</span>
                <span class="value">{{ upstreamText(site) }}</span>
                <span v-if="site.backends.length > 1" class="badge"
                    >共 {{ site.backends.length }} 个</span
                >
            </div>

            <!-- 第五行：操作按钮固定在卡片底部，保证多张卡片基线一致。 -->
            <div class="site-actions">
                <Button class="site-action" reverse-color @click="toggleEditWebsite(site)"
                    >编辑</Button
                >
                <Button class="site-action" reverse-color @click="confirmDelete(site)"
                    >删除</Button
                >
            </div>
        </Panel>
    </div>
    <Panel v-else>
        <div class="empty">
            <p class="empty-text">
                {{ keyword ? '没有匹配的网站' : '还没有网站' }}
            </p>
            <Button v-if="!keyword" type="button" @click="toggleAddWebsite"
                >添加第一个网站</Button
            >
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

/// 卡片上只展示主上游的地址（多后端时另有「共 N 个」徽标提示，见模板）。
/// 不再把数量拼进 URL 文本 —— 那会被 `text-overflow: ellipsis` 优先截掉，
/// 恰恰是"多上游"最需要提示的场景。
function upstreamText(site: Website): string {
    const main = site.backends?.find((b) => b.main) ?? site.backends?.[0];
    if (!main) return '未配置';
    return main.url;
}

/// 端口展示：443 标成 `443/HTTPS`（对齐参考 UI 的写法），其余原样列出。
function portsText(site: Website): string {
    const ports = site.ports ?? [];
    if (ports.length === 0) return '—';
    return ports
        .map((p) => (p === 443 ? '443/HTTPS' : String(p)))
        .join(' ');
}

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
    min-width: 0;
}

.search-input {
    width: 288px;
    max-width: 100%;
}

.count {
    white-space: nowrap;
}

/* 卡片网格：按宽度自适应列数，但**不缩到比卡片最小宽度更窄**。
   `minmax(min(320px, 100%), 1fr)` 里的 `min(..., 100%)` 是必要的 ——
   直接写 `minmax(320px, 1fr)` 时，容器窄于 320px 会横向溢出（`main` 又是
   `overflow-x: hidden`，表现为右侧被裁）。
   卡片内部用 flex column + `margin-top: auto` 把按钮压到底部，形成整齐的基线。 */
.websites {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(min(320px, 100%), 1fr));
    gap: 16px;
    align-items: stretch;
}

.site {
    display: flex;
    flex-direction: column;
    gap: 10px;
    min-width: 0;
    height: 100%;
    border-radius: 10px;
}

/* 第一行：状态标签 + 图标按钮（编辑入口），信息层级对齐参考 UI。 */
.site-top {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    min-width: 0;
}

.site-status {
    font-size: 0.75rem;
    line-height: 1.6;
    padding: 0 10px;
    border-radius: 4px;
    color: var(--main-color);
    border: 1px solid currentColor;
    opacity: 0.9;
    white-space: nowrap;
}

.site-edit-icon {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    padding: 0;
    border: 0;
    background: transparent;
    cursor: pointer;
    flex: 0 0 auto;
    opacity: 0.85;
}

.site-edit-icon:hover {
    opacity: 1;
}

.site-icon {
    width: 22px;
    height: 22px;
    fill: var(--main-color);
}

/* 第二行：标题（省略号）+ 详情。 */
.site-titlebar {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 12px;
    min-width: 0;
}

.site-name {
    font-weight: 600;
    font-size: 1rem;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
}

.site-detail {
    flex: 0 0 auto;
    padding: 0;
    border: 0;
    background: transparent;
    cursor: pointer;
    font: inherit;
    font-size: 0.8125rem;
    color: var(--main-color);
}

.site-detail:hover {
    text-decoration: underline;
}

/* 第三行：域名 / 端口，两列（标签列固定宽度，值列省略号）。 */
.site-fields {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr);
    column-gap: 10px;
    row-gap: 4px;
    margin: 0;
    font-size: 0.8125rem;
}

.site-fields dt {
    color: var(--main-color);
    opacity: 0.9;
    white-space: nowrap;
}

.site-fields dd {
    margin: 0;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
}

/* 第四行：两个指标并排，中间一条竖线（对齐参考 UI 的「今日请求 | 今日拦截」）。 */
.site-stats {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 8px;
    padding-top: 4px;
}

.stat {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
    text-align: center;
}

.stat + .stat {
    border-left: 1px solid rgba(127, 127, 127, 0.25);
}

.stat-label {
    font-size: 0.75rem;
    opacity: 0.6;
}

.stat-value {
    font-size: 1.125rem;
    font-weight: 700;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
}

.site-upstream {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
    font-size: 0.8125rem;
    padding-top: 4px;
    border-top: 1px solid rgba(127, 127, 127, 0.18);
}

.site-upstream .label {
    flex-shrink: 0;
    opacity: 0.6;
}

/* URL 自身可省略号截断；尾部徽标不参与收缩，保证"共 N 个"永远可见。 */
.site-upstream .value {
    flex: 1 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
}

.site-upstream .badge {
    flex: 0 0 auto;
    font-size: 0.75rem;
    padding: 0 8px;
    border-radius: 999px;
    background: rgba(127, 127, 127, 0.14);
    white-space: nowrap;
}

.site-actions {
    display: flex;
    gap: 8px;
    /* 让按钮在所有卡片里对齐到同一基线，即使上面的文字行数不同 */
    margin-top: auto;
    padding-top: 8px;
}

.site-action {
    flex: 1 1 0;
}

.empty {
    padding: 24px;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 16px;
}

.empty-text {
    margin: 0;
    opacity: 0.7;
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
}
</style>
