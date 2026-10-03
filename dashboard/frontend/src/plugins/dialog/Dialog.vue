<template>
    <div class="dialog-root" ref="dialogRootRef">
        <div class="dialog-backdrop" @click="requestClose"></div>
        <div class="dialog-container" @click.self="requestClose">
            <Panel class="panel">
                <div class="dialog-header" v-if="$slots.header">
                    <slot name="header"></slot>
                    <button
                        v-if="closable"
                        type="button"
                        class="dialog-close"
                        aria-label="关闭"
                        @click="requestClose"
                    >
                        <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
                            <path
                                d="M19 6.41 17.59 5 12 10.59 6.41 5 5 6.41 10.59 12 5 17.59 6.41 19 12 13.41 17.59 19 19 17.59 13.41 12z"
                            />
                        </svg>
                    </button>
                </div>
                <div class="dialog-content" v-if="$slots.content">
                    <slot name="content"></slot>
                </div>
                <div class="dialog-footer" v-if="$slots.footer">
                    <slot name="footer"></slot>
                </div>
            </Panel>
        </div>
    </div>
</template>

<script lang="ts" setup>
import { nextTick, onBeforeUnmount, onMounted, ref } from 'vue';
import Panel from '../../components/Panel.vue';

withDefaults(
    defineProps<{
        /**
         * 是否显示右上角关闭按钮 / 允许 ESC 关闭。
         *
         * 关闭请求一律走 `cancel` 事件（而不是 `close`）：这样父组件能先做
         * "有未保存内容"之类的拦截，`DialogContainer` 只在父组件没有处理时才移除弹窗。
         */
        closable?: boolean;
    }>(),
    { closable: true },
);

const emit = defineEmits(['cancel']);
const dialogRootRef = ref<HTMLDivElement | null>(null);

/**
 * 关闭请求的统一出口。
 *
 * 以前遮罩层直接 `$emit('close')`，会绕过调用方的 `cancel()`（`AddWebsite` 的
 * "已修改"确认就在 `cancel` 里），点一下弹窗外面改动就没了。
 */
function requestClose() {
    emit('cancel');
}

function onKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
        e.stopPropagation();
        requestClose();
    }
}

onMounted(() => {
    requestAnimationFrame(() => {
        nextTick(() => {
            dialogRootRef.value?.classList.add('dialog-animation');
        });
    });
    window.addEventListener('keydown', onKeydown);
});
onBeforeUnmount(() => {
    window.removeEventListener('keydown', onKeydown);
});
</script>

<style>
:root {
    --dialog-panel-shadow:
        0px 11px 15px -7px rgba(0, 0, 0, 0.2),
        0px 24px 38px 3px rgba(0, 0, 0, 0.12),
        0px 9px 46px 8px rgba(0, 0, 0, 0.12);
    --dialog-panel-color: rgb(255, 255, 255);
}
:root.dark {
    --dialog-panel-color: rgb(24, 24, 24);
}
</style>
<style scoped>
.dialog-root {
    position: fixed;
    z-index: 1300;
    inset: 0px;
}
.dialog-animation .dialog-backdrop {
    opacity: 1;
}
.dialog-out-animation .dialog-backdrop {
    opacity: 0;
}
.dialog-out-animation .dialog-container {
    opacity: 0;
    transform: scale(0.8);
}
.dialog-backdrop {
    position: fixed;
    display: flex;
    -webkit-box-align: center;
    align-items: center;
    -webkit-box-pack: center;
    justify-content: center;
    inset: 0px;
    background-color: rgba(0, 0, 0, 0.8);
    -webkit-tap-highlight-color: transparent;
    z-index: -1;
    opacity: 0;
    transition: opacity 225ms cubic-bezier(0.4, 0, 0.2, 1);
}
.dialog-container {
    height: 100%;
    outline: 0px;
    display: flex;
    -webkit-box-pack: center;
    justify-content: center;
    -webkit-box-align: center;
    align-items: center;
    /* 窄屏时容器自身可滚动，弹窗不会再被裁掉右侧内容。 */
    padding: 16px;
    box-sizing: border-box;
    overflow: auto;
}
/*
 * 弹窗宽度**取决于内容**（原来是固定 min-width: 480px + 两侧 32px margin，
 * 视口窄于 544px 时表单右侧被永久裁掉）。
 * 这里只设上下限：最宽 600px、最窄随视口收缩，且永远留出容器 padding。
 */
.panel {
    min-width: 0;
    width: min(600px, 100%);
    max-height: 100%;
    box-shadow: var(--dialog-panel-shadow);
    padding: 0;
    display: flex;
    flex-direction: column;
    height: auto;
    background-color: var(--dialog-panel-color);
    border-radius: 12px;
}
.dialog-header {
    margin: 0px;
    font-family: inherit;
    line-height: 1.6;
    flex: 0 0 auto;
    font-weight: 600;
    font-size: 16px;
    /* 与 content / footer 共用同一组左右内边距，标题、字段、按钮才会左对齐。 */
    padding: 24px 24px 12px;
    display: flex;
    -webkit-box-pack: justify;
    justify-content: space-between;
    -webkit-box-align: center;
    align-items: center;
    gap: 8px;
}
.dialog-header button.dialog-close {
    display: inline-flex;
    -webkit-box-align: center;
    align-items: center;
    -webkit-box-pack: center;
    justify-content: center;
    position: relative;
    box-sizing: border-box;
    -webkit-tap-highlight-color: transparent;
    background-color: transparent;
    cursor: pointer;
    user-select: none;
    vertical-align: middle;
    appearance: none;
    text-align: center;
    font-size: 1.5rem;
    color: var(--text-color);
    opacity: 0.55;
    outline: 0px;
    border-width: 0px;
    border-style: initial;
    border-color: initial;
    border-image: initial;
    margin: 0px;
    text-decoration: none;
    flex: 0 0 auto;
    padding: 6px;
    border-radius: 50%;
    transition:
        background-color 150ms cubic-bezier(0.4, 0, 0.2, 1),
        opacity 150ms;
}
.dialog-header button.dialog-close:hover {
    opacity: 1;
    background-color: rgba(127, 127, 127, 0.16);
}
.dialog-header .icon {
    user-select: none;
    width: 1em;
    height: 1em;
    display: inline-block;
    flex-shrink: 0;
    fill: currentcolor;
    font-size: 20px;
    transition: fill 200ms cubic-bezier(0.4, 0, 0.2, 1);
}
.dialog-content {
    width: 100%;
    /* flex 子项要能内部滚动，必须 min-height: 0（height: 100% 在 auto 容器里语义含糊）。 */
    flex: 1 1 auto;
    min-height: 0;
    overflow-y: auto;
    /* 左右与 header 对齐；上下由各表单自己的 gap 控制。 */
    padding: 0 24px 4px;
    box-sizing: border-box;
}
.dialog-footer {
    margin: 0px;
    font-family: inherit;
    line-height: 1.6;
    flex: 0 0 auto;
    font-weight: 600;
    font-size: 16px;
    padding: 8px 16px 16px;
    display: flex;
    -webkit-box-pack: justify;
    justify-content: space-between;
    -webkit-box-align: center;
    align-items: center;
}
</style>
