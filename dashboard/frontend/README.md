# WebGateway Console

Vue 3 + TypeScript + Vite control-plane panel.

## Pages

- Overview: site/certificate/gateway counts, today traffic and hourly chart.
- Sites: hosts, ports, backends and update time.
- Certificates: expiry state and hostnames.
- Gateways: online/offline nodes, heartbeat and version.
- Access logs: recent request/response rows with filters.
- Settings: account, theme, language and instance info.

## Development

Start the dashboard API (default port 3000) and the Vite dev server:

```bash
# terminal 1
DATABASE_URL='postgres://...' DASHBOARD_ADMIN_TOTP_SECRET=OBWGC2LOFVZXI4TJNZTS243FMNZGK5BNGEZDG cargo run -p dashboard

# terminal 2
cd dashboard/frontend
pnpm install
pnpm dev
```

Open http://localhost:5173 and sign in as `admin` with a TOTP code. The master
logs the default admin secret on first start; `DASHBOARD_ADMIN_TOTP_SECRET`
(base32) pins it for development. `DASHBOARD_API_TOKEN` remains available as a
service token for API clients. Vite proxies `/api` and `/health` to
`http://127.0.0.1:3000`.

## Build

```bash
pnpm install --frozen-lockfile
pnpm lint:check
pnpm build
```

The panel currently uses a small built-in history router and native `fetch`; it
does not add runtime dependencies beyond Vue.

For production static hosting, configure the web server to fall back to
`index.html` for unknown paths (SPA history mode).

## API

The master exposes read-only endpoints protected by a JWT from
`POST /api/auth/login` (username + TOTP). A service token can be supplied via
`DASHBOARD_API_TOKEN`:

- `GET /health`
- `GET /api/summary`
- `GET /api/websites`
- `GET /api/certificates`
- `GET /api/gateways`
- `GET /api/logs?limit=50`
