# shio-relay

内容无关的同步中继。规格以 [`docs/relay.md`](../../docs/relay.md) v0.2 为准；中继只维护 channel 的
payload 日志、设备游标与配额，不解析 payload。

## 构建与启动

```bash
cargo build -p shio-relay
./target/debug/relay --config relay.toml serve
# healthz
curl http://127.0.0.1:8080/healthz
```

```toml
# relay.toml
listen   = "127.0.0.1:8080"
data     = "/var/lib/shio-relay/relay.db"
frame_ttl_secs = 2592000        # 30 天
channel_quota_bytes = 104857600 # 100 MB
max_payload_bytes = 8388608     # 8 MiB
rate_limit_per_min = 60
```

## CLI

```bash
relay --config relay.toml channel new [--id <hex>] [--quota-bytes N] [--ttl-secs N]
relay --config relay.toml channel list

relay --config relay.toml device new [--device-id <hex>]   # 输出 token，仅一次
relay --config relay.toml token rotate --device <did>
relay --config relay.toml device revoke --device <did>

relay --config relay.toml member add --channel <cid> --device <did>
relay --config relay.toml member remove --channel <cid> --device <did>

relay --config relay.toml gc --dry-run
relay --config relay.toml gc --now
```

## HTTP API

除 `/healthz` 与 `/v1/enroll` 外都需要 `Authorization: Bearer <token>`。

| 方法 | 路径 | 作用 |
|---|---|---|
| `POST` | `/v1/channels/{cid}/frames` | 上传原始 payload；必带 `Idempotency-Key`，可选 `Content-Type` |
| `GET` | `/v1/channels/{cid}/frames?cursor=&limit=` | 拉取帧；默认排除自己 |
| `POST` | `/v1/channels/{cid}/ack` | `{"cursor":N}`；只前进 |
| `GET` | `/v1/channels/{cid}/info` | 游标、pending、quota、TTL、retained_from |
| `POST` | `/v1/channels/{cid}/enroll-codes` | 成员签发一次性入网码 |
| `POST` | `/v1/enroll` | `{"enroll_code","device_id"}` 换取新 device token |
| `POST` | `/v1/channels/{cid}/memberships` | 已认证 device 用入网码加入 channel |
| `DELETE` | `/v1/channels/{cid}/memberships/{did}` | 仅允许移除自己 |

## 说明

- `content_type` 只是 hint，不参与鉴权；relay 不解析 payload。
- token 是 device 级；membership 是 channel 级。
- `seq` 是传输顺序；`retained_from` 是仍可读的最小 `seq`。`retained_from > cursor + 1` 表示缺口。
- GC 只删连续前缀；配额压力会从最旧开始淘汰；幂等键 24h 后清空。
- 测试：`cargo test -p shio-relay`。
