import { createApp } from 'vue';
import App from './App.vue';
import './main.js';
import { router } from './constant.js';
import './theme';
createApp(App).use(router).mount('#app');
