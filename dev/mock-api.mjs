/**
 * 只读的**假后端**：给前端排版验收用（不需要数据库、不会碰 beta 库）。
 *
 * 为什么需要它：beta 库在做 90 天数据同步（COPY 持表锁）时，真正的 dashboard 后端
 * 一启动就要跑迁移建索引、会和同步抢表锁。这个 mock 只提供站点列表页所需的几个 GET
 * 接口并返回固定假数据，让「网站列表 + 编辑弹窗」能在真实前端里渲染出来对照 demo。
 *
 * 用法：
 *   node dev/mock-api.mjs 3010
 *   cd dashboard/frontend && BACKEND_URL=http://127.0.0.1:3010 npx vite --host 0.0.0.0 --port 5174
 */
import http from 'node:http';

const PORT = Number(process.argv[2] ?? 3010);

const websites = [
    {
        id: '66f000000000000000000001',
        name: '主站',
        hosts: ['www.example.com', 'example.com'],
        ports: [80, 443],
        certificates: [],
        backends: [
            { url: 'http://127.0.0.1:8080', balance: 10, main: true },
            { url: 'http://127.0.0.1:8081', balance: 5, main: false },
            { url: 'https://backend.internal:9443', balance: 1, main: false },
        ],
        config: {},
    },
    {
        id: '66f000000000000000000002',
        name: '',
        hosts: ['*'],
        ports: [443],
        certificates: [],
        backends: [
            {
                url: 'http://127.0.0.1:3000/a/very/long/upstream/path/that/should/be/truncated',
                balance: 0,
                main: true,
            },
        ],
        config: {},
    },
    {
        id: '66f000000000000000000003',
        name: '对象存储',
        hosts: ['s3.example.com', 'storage.example.com', 'cdn.example.com'],
        ports: [80, 443, 8080],
        certificates: ['cert-1'],
        backends: [{ url: 'http://10.240.0.9:9000', balance: 0, main: true }],
        config: {},
    },
    {
        id: '66f000000000000000000004',
        name: '一个名字非常非常长的站点用来验证标题省略号与卡片等高',
        hosts: ['very-long-subdomain.another-long-domain.example.com'],
        ports: [8443],
        certificates: [],
        backends: [{ url: 'http://127.0.0.1:9999', balance: 0, main: true }],
        config: {},
    },
];

const metrics = websites.map((w, i) => ({
    website_id: w.id,
    total_requests: [1234, 0, 98765, 42][i],
    total_responses: [1200, 0, 98000, 40][i],
    total_ips: 12,
    e4xx_requests: 3,
    e5xx_requests: 1,
    backend_error_requests: 0,
    total_requests_size: 1024 * 1024 * (i + 1),
    total_response_size: 1024 * 1024 * 8 * (i + 1),
}));

const routes = {
    '/websites': () => ({ code: 200, message: 'ok', data: websites }),
    '/access/metrics/websites': () => ({ code: 200, message: 'ok', data: metrics }),
    '/auth/check': () => ({ code: 200, message: 'ok', data: true }),
};

http.createServer((req, res) => {
    const url = new URL(req.url, 'http://localhost');
    const handler = routes[url.pathname];
    res.setHeader('Content-Type', 'application/json; charset=utf-8');
    if (!handler) {
        console.log(`[mock] ${req.method} ${url.pathname} -> 404`);
        res.statusCode = 404;
        res.end(JSON.stringify({ code: 404, message: 'mock: not found', data: null }));
        return;
    }
    console.log(`[mock] ${req.method} ${url.pathname} -> 200`);
    res.statusCode = 200;
    res.end(JSON.stringify(handler()));
}).listen(PORT, '0.0.0.0', () => {
    console.log(`[mock] listening on 0.0.0.0:${PORT}`);
});
