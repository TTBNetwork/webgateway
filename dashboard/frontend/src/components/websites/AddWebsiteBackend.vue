<template>
    <div class="backend">
        <InputEdit
            class="backend-url"
            label="回源地址"
            placeholder="http://127.0.0.1:8080"
            v-model:value="url"
        />
        <InputEdit
            class="backend-balance"
            label="权衡"
            type="number"
            v-model:value="balance"
        />
    </div>
</template>

<script setup lang="ts">
import InputEdit from '../InputEdit.vue';
const url = defineModel('url', {
    type: String,
    default: '',
});
const balance = defineModel('balance', {
    type: Number,
    default: 0,
});
</script>

<style scoped>
/*
 * 两列固定栅格：回源地址占满剩余宽度、权衡固定 120px。
 * 以前用 `style="width: auto"` + flex-shrink 分配，宽度取决于浏览器
 * `<input size=20>` 的固有宽度（实测约 360:144），换浏览器就错位。
 */
.backend {
    display: flex;
    flex-direction: row;
    align-items: center;
    gap: 16px;
    flex: 1 1 auto;
    min-width: 0;
}
.backend-url {
    flex: 1 1 auto;
    min-width: 0;
}
.backend-balance {
    flex: 0 0 120px;
    width: 120px;
}
</style>
