# shio-relay-client

`shio-sync` relay 适配层的真实 HTTP transport。基于 `reqwest::blocking` 实现
`RelayTransport`，负责 JSON/HTTP 与错误码映射；帧语义、`content_type` 分派和
gap 判断在 `shio_sync::relay::RelayClient`。

## 用法

```rust
use shio_relay_client::HttpRelayTransport;
use shio_sync::relay::{RelayClient, RelayError};
use shio_sync::Sync;

let transport = HttpRelayTransport::new("https://relay.example.com", token)?;
let mut client = RelayClient::new(Sync::new(&engine), transport);

// 推一页变更；空页返回 None。
if let Some(seq) = client.push_changes(after, 100)? {
    println!("pushed seq={seq}");
}

// 拉取并应用；gap 时调用方应触发快照重同步。
let report = client.pull_apply(cursor, 100)?;
if report.gap {
    // 让有完整数据的设备发布快照，然后从 retained_from 重拉。
    let report = client.pull_from_retained(100)?;
    cursor = report.next_cursor;
} else {
    cursor = report.next_cursor;
}

// 处理成功后推进 ack。
client.ack(cursor)?;
```

- `channel_id = hex(db_id)`；`RelayClient` 自动映射。
- 帧按 `content_type` 分派到 changes/offers/snapshot/progress 的 checked apply。
- 幂等键是帧的 SHA-256，重试同一帧会复用同一 key。
- HTTP 错误保留为 `RelayError::Http { status, code, message }`；网络/格式错误分别是
  `RelayError::Network` / `RelayError::Invalid`。
- 该 transport 是 **blocking**；在 async 应用里用 `spawn_blocking` 包一层。
- 真实 HTTP 端到端测试：`cargo test -p shio-relay-client`。
