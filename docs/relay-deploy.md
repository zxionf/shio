# relay 部署与运维

面向自建中继节点。传输规格见 [`relay.md`](relay.md) v0.2；本文只讲部署、TLS、备份与监控。

## 1. 目录与用户

```bash
sudo useradd --system --home /var/lib/shio-relay --shell /usr/sbin/nologin shio-relay
sudo install -d -o shio-relay -g shio-relay -m 0750 /var/lib/shio-relay /etc/shio-relay
sudo install -m 0755 target/release/relay /usr/local/bin/relay
```

- `/etc/shio-relay/relay.toml`：配置。
- `/var/lib/shio-relay/relay.db`：sqlite（WAL）。
- 数据目录只给 `shio-relay` 读写；配置只读。

## 2. relay.toml

```toml
listen   = "127.0.0.1:8080"
data     = "/var/lib/shio-relay/relay.db"
frame_ttl_secs = 2592000        # 30 天
channel_quota_bytes = 104857600 # 100 MB
max_payload_bytes = 8388608     # 8 MiB；SHY2 仍受内核 4 MiB 限制
rate_limit_per_min = 60
trust_proxy_headers = true        # 仅当反代可信并覆盖 XFF 时开启
```

- 公网只暴露 TLS 反代，`listen` 保持 `127.0.0.1`。
- `max_payload_bytes` 必须 ≥ 4 MiB + 4；客户端仍会按内核 4 MiB 上限生成 SHY2 帧。
- `trust_proxy_headers` 默认 `false`；关闭时忽略 `X-Forwarded-For` / `X-Real-IP`，
  enroll 只按 code 限流。只有反代**覆盖**这两个头时才开启。

## 3. systemd

`/etc/systemd/system/shio-relay.service`：

```ini
[Unit]
Description=shio relay
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=shio-relay
Group=shio-relay
Environment=RUST_LOG=info
ExecStart=/usr/local/bin/relay --config /etc/shio-relay/relay.toml serve
Restart=on-failure
RestartSec=3
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/shio-relay
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now shio-relay
sudo systemctl status shio-relay
```

## 4. 首次建库与成员

```bash
# shio 库：channel_id 直接用 db_id；非 db 内容可随机
sudo -u shio-relay relay --config /etc/shio-relay/relay.toml channel new --id <db_id>
sudo -u shio-relay relay --config /etc/shio-relay/relay.toml device new --device-id <device_id>
sudo -u shio-relay relay --config /etc/shio-relay/relay.toml member add --channel <channel_id> --device <device_id>
```

`device new` 输出的 token 只显示一次，交给客户端保存；不要把 token 写进 URL、日志或配置文件。

## 5. TLS 反代

### Caddy（推荐，自动 ACME）

`/etc/caddy/Caddyfile`：

```caddyfile
relay.example.com {
    encode zstd gzip
    header {
        Strict-Transport-Security "max-age=15552000; includeSubDomains"
        X-Content-Type-Options "nosniff"
    }
    reverse_proxy 127.0.0.1:8080
}
```

### nginx

```nginx
server {
    listen 80;
    server_name relay.example.com;
    return 301 https://$host$request_uri;
}

server {
    listen 443 ssl;
    http2 on;
    server_name relay.example.com;

    ssl_certificate     /etc/letsencrypt/live/relay.example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/relay.example.com/privkey.pem;
    ssl_protocols TLSv1.2 TLSv1.3;

    add_header Strict-Transport-Security "max-age=15552000; includeSubDomains" always;
    add_header X-Content-Type-Options nosniff always;
    client_max_body_size 9m;

    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_request_buffering off;
    }
}
```

- 最低 TLS 1.2（优先 1.3）；HSTS `max-age=15552000`。
- 反代日志不要记录 `Authorization`；IP 只保留 7 天。

## 6. 备份与恢复

- `relay.db` 可丢：shio 库由任一设备发布快照恢复；非 shio 内容由应用重传。
- 在线备份：`sqlite3 /var/lib/shio-relay/relay.db ".backup '/var/backups/relay.db'"`。
- 停机备份：先 `systemctl stop shio-relay`，再复制 `relay.db` 及 `-wal`/`-shm`。
- 恢复后 `chown shio-relay:shio-relay`，再启动；设备 token 与 membership 会随库恢复。

## 7. 监控与 GC

- 健康检查：`curl -fsS http://127.0.0.1:8080/healthz` 或经 TLS 的 `https://relay.example.com/healthz`。
- 日志：`journalctl -u shio-relay -f`；relay 不记 token、payload、content_type；
  请求日志只记 method/path/status/耗时。
- 磁盘/配额：`relay --config ... channel list`；`relay --config ... stats --channel <cid>`
  查看 seq、字节、quota 与成员 ack；`--json` 输出可解析 JSON，便于监控采集；
  `relay --config ... gc --dry-run` 先看淘汰量，再 `gc --now`。
- 每日 GC timer（示例）：

```ini
# /etc/systemd/system/shio-relay-gc.service
[Unit]
Description=shio relay GC

[Service]
Type=oneshot
User=shio-relay
ExecStart=/usr/local/bin/relay --config /etc/shio-relay/relay.toml gc --now
```

```ini
# /etc/systemd/system/shio-relay-gc.timer
[Unit]
Description=Daily shio relay GC

[Timer]
OnCalendar=daily
Persistent=true

[Install]
WantedBy=timers.target
```

```bash
sudo systemctl enable --now shio-relay-gc.timer
```

- 磁盘满时 relay 返回 `507 storage_full`；先检查 quota/磁盘，再手动 `gc --now`。

## 8. 安全清单

- 公网必须 TLS；不要在公网明文监听。
- token 只走 `Authorization: Bearer`；不要放 URL 参数。
- 全局撤销设备用 CLI：`relay --config ... device revoke --device <did>`。
- membership 移除用 CLI：`relay --config ... member remove --channel <cid> --device <did>`。
- 定期检查 `systemctl status`、`journalctl`、`channel list`、`gc --dry-run`。

## 9. 当前实现边界

- 内置 TLS 未实现；按本文用 Caddy/nginx。
- 限流：per-token 每分钟固定窗口 + 并发 4；`/v1/enroll` 按 code 10/min，
  仅当 `trust_proxy_headers = true` 时再按 IP 30/min（`X-Forwarded-For`/`X-Real-IP`）。

## 10. 本地冒烟

```bash
cargo build -p shio-relay

dir=$(mktemp -d)
cat > "$dir/relay.toml" <<EOF
listen = "127.0.0.1:8080"
data = "$dir/relay.db"
max_payload_bytes = 1024
channel_quota_bytes = 4096
EOF

./target/debug/relay --config "$dir/relay.toml" channel new --id cccccccccccccccccccccccccccccccc
./target/debug/relay --config "$dir/relay.toml" device new --device-id dddddddddddddddddddddddddddddddd
./target/debug/relay --config "$dir/relay.toml" member add \
  --channel cccccccccccccccccccccccccccccccc \
  --device dddddddddddddddddddddddddddddddd
./target/debug/relay --config "$dir/relay.toml" channel list
./target/debug/relay --config "$dir/relay.toml" gc --dry-run

./target/debug/relay --config "$dir/relay.toml" serve &
curl -fsS http://127.0.0.1:8080/healthz
# ok
kill %1
```

- `device new` 的 token 只显示一次；用同一个 token 调 `/v1/channels/{cid}/frames` 验证推帧。
- `channel list` 应看到 `next=1 retained=1 bytes=0`。
- `gc --dry-run` 应输出 `deleted=0`（空 channel）。
