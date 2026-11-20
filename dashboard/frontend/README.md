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
(base32) pins it for development. Vite proxies `/api` and `/health` to
`http://127.0.0.1:3000`, which keeps the panel and the API on one origin so the
session cookie works. Service tokens for API clients are configured with
`DASHBOARD_API_TOKENS` (comma-separated; `DASHBOARD_API_TOKEN` is the legacy
single-token form).

## Build

```bash
pnpm install --frozen-lockfile
pnpm lint:check
pnpm build
```

The panel currently uses a small built-in history router and native `fetch`; it
does not add runtime dependencies beyond Vue.

The dashboard can host this build directly: set `DASHBOARD_STATIC_DIR` to the
`dist/` directory and it serves the panel with SPA history fallback, a panel CSP,
hashed-asset caching and the same hardening headers as the API. This keeps the
panel and `/api` on one origin, which the session cookie requires. Deploying the
build behind an external web server instead works too; see
[deployment.md](../../deployment.md) for the nginx/systemd setup, but the proxy
must serve `/api` and `/health` from the same origin and reproduce the security
headers.

## Security notes

- `POST /api/auth/login` sets the session JWT in the `wg_session` cookie
  (HttpOnly, SameSite=Strict, `Max-Age` = session TTL). The JSON body only
  returns the user, so scripts can never read the token. The panel restores the
  session with `GET /api/auth/me` on boot, which requires the panel and the API
  to share an origin.
- Logout and admin revocation bump `users.token_version`, which invalidates
  every session of that user immediately.
- Every API response carries a deny-all CSP, `X-Content-Type-Options: nosniff`,
  `X-Frame-Options: DENY`, `Referrer-Policy: no-referrer` and
  `Cache-Control: no-store`. A TLS-terminating proxy should send
  `X-Forwarded-Proto: https` so the dashboard also marks the cookie `Secure`
  and emits `Strict-Transport-Security`.
- Internal failures are logged with an `error_id`; clients receive
  `{"error":"internal server error","error_id":"..."}` and `GET /health` does
  not expose database error details.

## API

The master exposes read-only endpoints protected by the session cookie from
`POST /api/auth/login` (username + TOTP) or, for machine clients, by a service
token sent as `Authorization: Bearer <token>` (configure `DASHBOARD_API_TOKENS`,
several entries are accepted during rotation):

- `GET /health`
- `GET /api/summary`
- `GET /api/websites`
- `GET /api/certificates`
- `GET /api/gateways`
- `GET /api/logs?limit=50`
