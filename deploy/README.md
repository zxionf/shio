# deploy

可直接复制的部署示例。完整说明见 [`docs/relay-deploy.md`](../docs/relay-deploy.md)。

- `relay.toml.example` → `/etc/shio-relay/relay.toml`
- `shio-relay.service` → `/etc/systemd/system/shio-relay.service`
- `shio-relay-gc.service` / `shio-relay-gc.timer` → `/etc/systemd/system/`
- `Caddyfile` → `/etc/caddy/Caddyfile`（自动 TLS）
- `nginx.conf` → `/etc/nginx/conf.d/relay.conf`（需自备证书）

Caddy/nginx 示例都**覆盖** `X-Forwarded-For`，只有在这种反代后面才能把
`trust_proxy_headers` 设为 `true`。
