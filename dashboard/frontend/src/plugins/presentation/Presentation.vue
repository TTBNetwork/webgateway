<template>
    <div class="presentation-container">
        <div
            class="presentation-msg"
            :class="`${msg.type}`"
            v-for="msg in presentations"
            :key="msg.id"
            @click="removePresentation(msg.id)"
        >
            <div class="icon">
                <SvgIcon
                    :name="`presentation-${msg.type}`"
                    class="svg"
                    size="1.375em"
                ></SvgIcon>
            </div>
            <div class="text">{{ msg.message }}</div>
        </div>
    </div>
</template>

<script setup lang="ts">
import SvgIcon from '../../components/SvgIcon.vue';
import { presentations, removePresentation } from './index';
</script>

<style>
:root {
    --presentation-shadow:
        0px 12px 24px -4px rgba(0, 4, 10, 0.06),
        0px 0px 2px 0px rgba(0, 4, 10, 0.06);
    --presentation-msg-alert-color: rgb(102, 65, 56);
    --presentation-msg-alert-bg-color: rgb(255, 245, 243);
    --presentation-msg-success-color: rgb(68, 96, 50);
    --presentation-msg-success-bg-color: rgb(246, 253, 242);
}
:root.dark {
    --presentation-shadow:
        0px 12px 24px -4px rgba(0, 4, 10, 0.06),
        0px 0px 2px 0px rgba(0, 4, 10, 0.06);
    --presentation-msg-alert-color: rgb(255, 218, 209);
    --presentation-msg-alert-bg-color: rgb(25, 16, 14);
    --presentation-msg-success-color: rgb(200, 230, 180);
    --presentation-msg-success-bg-color: rgb(28, 45, 20);
}
</style>
<style scoped>
.presentation-container {
    z-index: 1400;
    position: fixed;
    display: flex;
    left: 8px;
    right: 8px;
    flex-direction: column;
    -webkit-box-pack: center;
    justify-content: center;
    -webkit-box-align: center;
    align-items: center;
    top: 8px;
}
.alert.presentation-msg {
    color: var(--presentation-msg-alert-color);
    background-color: var(--presentation-msg-alert-bg-color);
}
.success.presentation-msg {
    color: var(--presentation-msg-success-color);
    background-color: var(--presentation-msg-success-bg-color);
}
.presentation-msg {
    box-shadow: var(--presentation-shadow);
    font-family: inherit;
    font-weight: 400;
    font-size: 0.875rem;
    line-height: 1.43;
    display: flex;
    margin-bottom: 10px;
    transition: box-shadow 300ms cubic-bezier(0.4, 0, 0.2, 1);
    border-radius: 4px;
    padding: 6px 16px;
    align-items: center;
}
.alert .icon {
    color: rgb(255, 77, 79);
}
.icon {
    margin-right: 12px;
    padding: 7px 0px;
    display: flex;
    font-size: 22px;
    opacity: 0.9;
}
.svg {
    user-select: none;
    width: 1em;
    height: 1em;
    display: inline-block;
    flex-shrink: 0;
    fill: currentcolor;
    font-size: inherit;
    transition: fill 200ms cubic-bezier(0.4, 0, 0.2, 1);
}
.text {
    padding: 6px 0px;
    min-width: 0px;
    overflow: auto;
}
</style>
