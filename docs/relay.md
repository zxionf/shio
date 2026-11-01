# 同步中继（relay）规格

> 状态：**v0.2**（2026-10-31）。内容无关的中继节点。  
> v0.2 相对 v0.1：中继不再解析 `SHY2` 帧头；`db_id` → `channel_id`；每 device 一个 token + membership；`content_type` 仅作 hint；帧校验集中到内核侧（`shio-core`）。  
> 内核侧同步语义以 `.design/同步.md`、`SPEC.md`、`api.md` 为准；本文只规定**传输节点**。  
> 客户端（Android 应用）的取舍与威胁模型见 oxygen 仓库 `.design/sync-design.md`。

## 1. 定位

中继是**与业务无关的、认证后的 payload 日志 + 路由器**：按 `channel_id` 维护一条只追加的 payload 日志，客户端推 payload、拉 payload、回报游标。

- 不解析 payload。relay 不知道里面是 `SHY2` 帧、附件还是任意字节。
- 不持密钥、不合并、不做业务语义。
- 每个 shio 库对应一个 channel；密码库、笔记、待办、附件共用同一台中继。
- `content_type` 是客户端提供的 hint，relay 只存储和原样返回，不做安全判断。
- `SHY2` 的 magic/format/kind/db_id/sender 校验由内核侧统一完成，relay 不复制。

### 与「中继 = 一个成员节点」的比较

`.design/同步.md` §9 倾向「中继 = 一个成员（持有库、参与合并）」。本项目**选纯中继**，理由：

- 中继不持有任何库文件与主口令，被拖库只泄露密文 payload 与元数据；
- 中继逻辑与业务解耦，升级同步语义不需要改中继；
- 代价：中继要维护每 channel 的日志与每成员 ack，但这是一张表的事，不构成对等成员那样的复杂度。

## 2. 与内核的分工

| 层 | 负责 |
|---|---|
| relay | 认证、channel、payload 大小、幂等、`seq`、`ack`、quota/TTL/GC、限流 |
| 内核侧（`shio-core`） | `SHY2` 帧头校验：长度、magic、format、kind、`db_id`、`sender`、flags、条目边界；记录 blob 校验；progress 语义 |
| 客户端 | `db_id` ↔ `channel_id` 映射、上传/下载、按 `content_type` 分派、重同步决策、ack 时机 |

**唯一校验入口**：内核侧提供 `validate_frame(frame, expected_db_id, expected_sender)`（名称以实现为准），由 `shio-sync` 的 `decode_*` / `apply*` 路径和客户端复用。上传前客户端用 `expected_sender = 本机 device_id`；下载后用 `expected_sender = relay frame.sender`。relay 不实现、不调用这个入口。实现上可以把帧字节布局继续留在 `shio-sync`，但对外只暴露一个校验函数，调用方不重复实现规则。

当前 `shio-sync` 的 `decode_*` 已校验 magic/format/kind/db_id，但**未校验帧头 `sender`**；实现 relay 客户端前需要把这个校验补进内核侧的唯一入口。relay 本身不做这件事。

**为什么 sender 必须校验**：内核的 `apply_page` / `apply_progress` 会用帧头 `sender` 写 `peer/`、`cur/`、`heard/` 等设备级状态。如果客户端不比对 relay 侧 `sender`，一个 bug 或恶意成员可以伪造 `sender=Y` 的帧，让对端误以为 Y 已读到某处，导致日志被提前裁剪或对端状态错乱。内容无关中继把这个校验从传输层移到端点层，但必须补上。

## 3. 数据模型

sqlite，WAL 单文件；时间用 Unix 秒。

```sql
CREATE TABLE channels (
  channel_id        TEXT PRIMARY KEY,   -- 16 字节 hex；shio 库直接用 db_id
  next_seq          INTEGER NOT NULL,   -- 下一个分配的 seq，从 1 开始
  retained_from     INTEGER NOT NULL,   -- 最小仍可读的 seq；空 channel = next_seq
  bytes             INTEGER NOT NULL DEFAULT 0,
  quota_bytes       INTEGER NOT NULL,
  frame_ttl_secs    INTEGER,            -- NULL 表示用全局默认
  max_payload_bytes INTEGER,            -- NULL 表示用全局默认
  created_at        INTEGER NOT NULL
);

CREATE TABLE frames (
  channel_id   TEXT NOT NULL,
  seq          INTEGER NOT NULL,        -- 每 channel 单调递增，由 relay 分配
  sender       TEXT NOT NULL,           -- 来自 token，不来自 payload
  content_type TEXT NOT NULL,           -- hint，默认 application/octet-stream
  payload      BLOB NOT NULL,
  created_at   INTEGER NOT NULL,
  idem_key     TEXT,                    -- 24h 后由 GC 清空
  PRIMARY KEY (channel_id, seq)
);
CREATE UNIQUE INDEX frames_idem ON frames(channel_id, sender, idem_key)
  WHERE idem_key IS NOT NULL;
CREATE INDEX frames_created ON frames(channel_id, created_at);

CREATE TABLE devices (
  device_id   TEXT PRIMARY KEY,         -- 16 字节 hex，客户端生成
  token_hash  TEXT NOT NULL UNIQUE,     -- SHA-256(token)
  created_at  INTEGER NOT NULL,
  last_seen   INTEGER,
  revoked_at  INTEGER
);

CREATE TABLE memberships (
  channel_id   TEXT NOT NULL,
  device_id    TEXT NOT NULL,
  acked_cursor INTEGER NOT NULL DEFAULT 0,
  created_at   INTEGER NOT NULL,
  revoked_at   INTEGER,
  PRIMARY KEY (channel_id, device_id)
);

CREATE TABLE enroll_codes (
  code_hash  TEXT PRIMARY KEY,          -- SHA-256(code)
  channel_id TEXT NOT NULL,
  expires_at INTEGER NOT NULL,
  used_at    INTEGER,
  created_at INTEGER NOT NULL
);
```

- `channel_id` 固定 16 字节 hex；shio 场景直接复用 `db_id`，客户端负责映射，relay 不校验。
- `frames.sender` 一律来自令牌；payload 里的 sender 与 relay 无关。
- `content_type` 只做 hint；最长 128 字节，只允许可见 ASCII，不参与鉴权。
- `idem_key` 由 API 强制必填；24h 后 GC 置空，之后同 key 视为新请求。
- `retained_from` 是单一前缀边界；GC 只删除连续前缀。

## 4. API v1

除 `/healthz` 与 `/v1/enroll` 外都需要 `Authorization: Bearer <token>`。channel 在 path 里；token 是 device 级。

| 方法 | 路径 | 作用 |
|---|---|---|
| `POST` | `/v1/channels/{channel_id}/frames` | 上传 1 个 payload（body = 原始字节）；必带 `Idempotency-Key`；可选 `Content-Type`；返回 `{"seq":N}` |
| `GET` | `/v1/channels/{channel_id}/frames?cursor=&limit=` | 拉 `seq > cursor` 的帧；默认排除 `sender == 自己`；返回 `{"frames":[{"seq","sender","content_type","payload_b64"}],"next_cursor","has_more","retained_from"}` |
| `POST` | `/v1/channels/{channel_id}/ack` | body `{"cursor":N}`；只前进不回退 |
| `GET` | `/v1/channels/{channel_id}/info` | `{"channel_id","device_id","acked_cursor","next_seq","pending_frames","pending_bytes","quota_bytes","frame_ttl_secs","retained_from","max_payload_bytes","relay_version"}` |
| `POST` | `/v1/channels/{channel_id}/enroll-codes` | 活跃成员签发一次性入网码 `{"enroll_code","expires_at"}`；TTL 10 分钟 |
| `POST` | `/v1/enroll` | 无鉴权；body `{"enroll_code","device_id"}`；新设备 → `{"token","device_id","channel_id"}`；device 已存在 → `409 device_exists` |
| `POST` | `/v1/channels/{channel_id}/memberships` | 已认证 device 用 `{"enroll_code"}` 加入新 channel；消耗入网码；返回 `{"channel_id","device_id"}` |
| `DELETE` | `/v1/channels/{channel_id}/memberships/{device_id}` | 仅允许删除自己（离开 channel）；删除他人返回 `403`，由 CLI 处理 |
| `GET` | `/healthz` | `ok`，无任何 channel 信息 |

v1 **不提供 HTTP 建 channel**：channel 由运维 CLI 创建，避免客户端抢注 `channel_id`。全局撤销设备也只走 CLI，避免任一成员通过 HTTP 踢掉整个设备。

`enroll_code` 的 channel 必须与 path 一致；`POST /v1/enroll` 用 code 内嵌的 channel。device 已存在时返回 `409`，客户端应改用已有 token 调 `POST /v1/channels/{channel_id}/memberships`。

### 凭据

- `device_id`：16 字节随机 hex，客户端生成。
- `token`：32 字节随机，base64url；只存 SHA-256。
- `enroll_code`：32 字节随机，base64url；只存 SHA-256；10 分钟有效、一次性。
- `channel_id`：16 字节 hex；shio 库直接用 `db_id`。

### 校验规则（只有传输层）

1. token 有效、device 未全局撤销 → `device_id`；
2. path 的 channel 存在，且该 device 的 membership 活跃 → 否则 `404 channel_not_found` / `403 not_a_member`；
3. payload 非空且 ≤ channel/global `max_payload_bytes` → 否则 `400 empty_payload` / `413 payload_too_large`；
4. `Content-Type` 为空时取 `application/octet-stream`；长度 ≤ 128，只允许可见 ASCII → 否则 `400 invalid_content_type`；
5. `Idempotency-Key` 存在，长度 ≤ 128 → 否则 `400 missing_idempotency_key`；同 `(channel, sender, key)` 重试：payload 与 `content_type` 相同返回原 `seq`，不同返回 `409 idempotency_conflict`；
6. `ack` 的 `cursor` ≤ `next_seq - 1` → 否则 `400 invalid_cursor`；
7. **不解析 payload**；不检查 magic/format/kind/db_id/sender。

### 错误码

`invalid_token`(401)、`not_a_member`(403)、`channel_not_found`(404)、`empty_payload` / `invalid_content_type` / `missing_idempotency_key` / `invalid_cursor`(400)、`idempotency_conflict` / `device_exists`(409)、`enroll_code_used` / `enroll_code_expired`(410)、`payload_too_large`(413)、`rate_limited`(429)、`storage_full`(507)。

## 5. 游标、ack 与 GC

- `seq` 由 relay 分配，每 channel 单调递增、无空洞；与内核的 `after` 是两套东西：前者是传输顺序，后者是库内日志位置。
- `acked_cursor` 每 membership 一份；只前进不回退。
- `retained_from` = 最小仍可读的 `seq`；空 channel 等于 `next_seq`。
- 客户端有 `cursor`；若 `retained_from > cursor + 1`，说明中间已丢帧，进入重同步：
  - shio 库：内核全量快照（`snapshot_*`），成功后从 `retained_from` 重新拉；重复帧由内核幂等吸收；
  - 非 shio 内容：应用层决定重传、重建或放弃。
- GC 只删除**连续前缀**，保证 `retained_from` 是一个边界。对前缀里的帧 `S@seq`，满足任一即可删：
  1. 超过 `frame_ttl_secs`；
  2. channel `bytes` 超 `quota_bytes`，从最旧开始淘汰；
  3. 存在其它活跃成员，且这些成员的 `acked_cursor` 都 `>= seq`。
- 条件 3 **不要求帧的发送者自己 ack**；否则单设备 channel 永远不 GC。没有其它活跃成员时，不因 ack 删除，只靠 TTL/quota（保留历史给将来加入的设备）。
- `active member` = `memberships.revoked_at IS NULL` 且 `devices.revoked_at IS NULL`。
- `pending_frames` / `pending_bytes` 按 device 计：`seq > acked_cursor` 且 `sender != device_id`。
- `Idempotency-Key` 24h 后清空；不影响帧本身。过期/已用的 `enroll_codes` 由 GC 清理。

## 6. 并发、幂等与限流

- 同一 channel 写入串行（sqlite `BEGIN IMMEDIATE`），保证 `seq` 严格递增且无空洞；不同 channel 共享 sqlite 单写者。
- `Idempotency-Key` 24h 内去重；重试返回原 `seq`，不产生重复帧；同 key 不同内容返回 `409`。
- 限流：每 token 每 channel 60 请求/分钟、并发 4；另设每 token 全局上限（默认 240/分钟）；`/v1/enroll` 另按 IP 和 code 限流；超限 `429` + `Retry-After`。
- `GET`：`limit` 默认 100、上限 1000；另有响应字节预算（默认 8 MiB，按 base64 后大小计；单帧超过预算时仍单独返回）；没有可返回帧时 `next_cursor = cursor`。
- relay 只保证「每设备自身顺序 + 每 channel `seq` 单调」，不保证跨设备因果序。

## 7. TLS 与卫生

- 公网必须 TLS：Caddy/nginx（推荐，ACME）或内置 rustls；禁止明文 HTTP 监听。
- 最低 TLS 1.2（优先 1.3）；HSTS `max-age=15552000`。
- 令牌只走 `Authorization`，禁止 URL 参数。
- 日志不记令牌、不记 payload、不记 `content_type`；只记 `channel_id`、`seq`、大小、状态码、耗时；IP 只保留 7 天。
- token 是 device 级：泄露会影响该设备加入的所有 channel；v1 用全局撤销收口，需要更细粒度时再加 per-channel token。
- relay 不签发证书、不做证书固定。

## 8. 部署与运维

```toml
# relay.toml
listen   = "127.0.0.1:8080"
data     = "/var/lib/shio-relay/relay.db"
frame_ttl_secs = 2592000        # 30 天
channel_quota_bytes = 104857600 # 100 MB
max_payload_bytes = 8388608     # 8 MiB；shio 帧仍受内核 4 MiB 限制
rate_limit_per_min = 60
```

```bash
relay serve --config relay.toml

# 建 channel：shio 库用 --id <db_id>；其它内容可随机
relay channel new [--id <hex>] [--quota-bytes N] [--ttl-secs N]

# 设备与成员
relay device new [--device-id <hex>]        # 输出 token，只显示一次
relay token rotate --device <did>           # 生成新 token，旧 token 立即失效
relay member add --channel <cid> --device <did>
relay member remove --channel <cid> --device <did>
relay device revoke --device <did>          # 全局撤销

relay stats --channel <cid>                 # seq 范围、帧数、字节数、成员 ack 表、content_type 直方图
relay gc --dry-run|--now                    # 按 5 的规则清理；顺带清过期入网码和 24h 前的 idem_key
```

- 单实例 + systemd；不做集群、不做多租户控制台。
- 备份可选：`relay.db` 可丢——shio 库由任一设备发布快照恢复；非 shio 内容由应用重传或接受丢失。

## 9. 失败与恢复

| 情形 | 处理 |
|---|---|
| 客户端超时/`5xx` | 同一 `Idempotency-Key` 重试（指数退避，最多 5 次） |
| 令牌撤销 | `401` → 重新配对 |
| membership 被移除 | `403 not_a_member` → 重新入网或换 channel |
| `retained_from > cursor + 1` | shio：快照重同步；非 shio：应用层重传/重建 |
| relay 整库丢失 | shio：任一设备发布快照；非 shio：重传或接受丢失 |
| 磁盘满 | 先按 GC 淘汰最旧帧；仍写不下 `507` + `/v1/info` 报警 |
| 入网码过期/已用 | `410`；重新签发 |

**relay 不是备份**：payload 是传输中间态，随时可丢；真正的副本在各设备。

## 10. 多应用与附件

- 每个 shio 库一个 channel，`channel_id = db_id`；附件默认放同一个 channel，用 `content_type` hint 区分。
- 需要不同 TTL/quota 的附件可以单独建 channel；这是客户端/应用的选择，relay 无感。
- token 是 device 级，membership 是 channel 级：一台设备一个 token，可加入多个 channel。
- 一个应用同步多个库，就是多个 channel + 多个 membership；relay 不区分应用类型。

## 11. 非目标

- 不做服务端解密、搜索、去重、合并、payload 校验；
- 不做账号体系、推送通道、设备发现；
- 不做 WebSocket/长连接；
- 不做 HA/分片；
- 不做角色/最小权限：v1 中任一活跃成员可签发入网码；移除他人成员和全局撤销设备只走 CLI。入网码只授予 relay 访问，不授予库密钥。

## 12. 客户端契约（shio）

1. `channel_id = hex(db_id)`；一个库一个 channel。
2. 上传前：`validate_frame(payload, db_id, 本机 device_id)`；不通过不上传。
3. 下载后：`validate_frame(payload, db_id, relay_frame.sender)`；不通过不 `apply`。
4. `progress` 帧同样走 3；只有 `header.sender == relay_frame.sender` 才能 `apply_progress`。
5. `content_type` 只用来分派（例如 `application/vnd.shio.shy2` vs 附件），不能作为安全依据。
6. 收到 `retained_from > cursor + 1`：先快照重同步，再从 `retained_from` 重拉；内核 `apply` 幂等。
7. ack 只在处理成功后推进；不要用 relay `seq` 当内核 `after`。
8. 重试必须复用 `Idempotency-Key`；同 key 换 payload 会被 `409`。

## 13. 测试计划

- 单元：token/membership 鉴权、`seq` 单调、幂等同/异 payload、`content_type` 长度、payload 上限、GC 前缀/TTL/quota、`retained_from`、入网码一次性/过期。
- 集成：两个 device 各自 token、一个 device 多 channel、加入/离开 channel、推/拉/ack、排除自己、`progress` 帧、gap 检测。
- 内核侧：`validate_frame` 覆盖 magic/format/kind/db_id/sender/flags/条目边界；sender 不匹配拒绝；progress 伪造拒绝。
- 端到端：Android ↔ 中继 ↔ 第二台设备跑 changes/apply 往返。
