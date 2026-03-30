import ky from 'ky';
import type { APIResponse, AuthResponse } from './types';
import { createRouter, createWebHistory } from 'vue-router';
import { checkToken, getToken } from './auth';

let inputId = 0;
export function increaseInputID() {
    return inputId++;
}

export const got = ky.create({
    prefixUrl: new URL('/api', window.location.origin).toString(),
    throwHttpErrors: false,
    hooks: {
        afterResponse: [
            async (_, __, response) => {
                let resp: APIResponse = {
                    status: response.status,
                    message: response.statusText,
                    data: null,
                };
                try {
                    resp = await response.json();
                } catch (e) {}
                if (
                    response.headers.has('Refresh-Token') &&
                    response.headers.has('Refresh-Token-Expired')
                ) {
                    const token = response.headers.get(
                        'Refresh-Token',
                    ) as string;
                    const expired = response.headers.get(
                        'Refresh-Token-Expired',
                    ) as string;
                    const auth: AuthResponse = {
                        token,
                        exp_at: expired,
                    };
                    localStorage.setItem('token', JSON.stringify(auth));
                }
                return new Response(JSON.stringify(resp), response);
            },
        ],
    },
});

export const gotWithAuth = got.extend({
    hooks: {
        beforeRequest: [
            async (request) => {
                const token = getToken();
                if (token) {
                    request.headers.set('Authorization', `Bearer ${token}`);
                }
            },
        ],
    },
});

export const router = createRouter({
    history: createWebHistory(),
    routes: [
        {
            path: '/login',
            component: () => import('./pages/Auth.vue'),
        },
        {
            // 泛匹配 /xxx/xxx
            path: '/:pathMatch(.*)*',
            component: () => import('./pages/Dashboard.vue'),
            meta: {
                requiresAuth: true,
            },
            children: [
                {
                    path: 'statistics',
                    component: () => import('./pages/Dashboard/Statistics.vue'),
                },
                {
                    path: 'settings',
                    component: () => import('./pages/Dashboard/Settings.vue'),
                },
                {
                    path: 'websites',
                    component: () => import('./pages/Dashboard/websites.vue'),
                },
                {
                    path: 'websites/certificates',
                    component: () =>
                        import('./pages/Dashboard/websites/CertificateManager.vue'),
                },
            ],
        },
    ],
});

router.beforeEach(async (to, _, next) => {
    if (to.meta.requiresAuth && !(await checkToken())) {
        next('/login');
    } else {
        next();
    }
});

export function pushQuery(query: Record<string, any>) {
    router.push({
        path: router.currentRoute.value.path,
        query: {
            ...router.currentRoute.value.query,
            ...query,
        },
    });
}

export const lightMainColor = '#0FC6C2';
export const darkMainColor = '#f4d1b4';
