# 同步中继（relay）规格

> 状态：**规格草案 v0.1**（2026-10-31）。面向「自建中继节点」的实现者。
> 内核侧的同步语义以 `.design/同步.md`、`SPEC.md`、`api.md` 为准；本文只规定**传输节点**。
> 客户端（Android 应用）的取舍与威胁模型见 oxygen 仓库 `.design/sync-design.md`。

## 1. 定位

中继是**与业务无关的帧信箱 + 路由器**：按 `db_id` 维护一条只追加的帧日志，客户端推帧、拉帧、回报游标。

- 不解析帧内容（除 77 字节帧头里的 `db_id`/`sender`/`kind` 用于路由与校验）；
- 不持密钥、不合并、不做业务语义；
- 密码库、笔记、待办共用同一台中继：对中继来说都只是「某个 `db_id` 的帧」。

### 与「中继 = 一个成员节点」的比较

`.design/同步.md` §9 倾向「中继 = 一个成员（持有库、参与合并）」。本项目**选纯中继**，理由：

- 中继不持有任何库文件与主口令，被拖库只泄露密文帧与元数据；
- 中继逻辑与业务解耦，升级同步语义不需要改中继；
- 代价：中继要维护每 `db_id` 的日志与每设备的 ack，但这是一张表的事，不构成对等成员那样的复杂度。

## 2. 帧的事实（来自内核，供实现者参考）

- 帧头固定 77 字节，`magic = b"SHY2"`，`format = 2`；
- 帧头含：`kind`（`0=changes`/`1=offers`/`2=snapshot`/`3=progress`）、`db_id`（16 字节）、`sender`（device id）、
  `after`、`log_start`、`log_end`、`next`、`count`；
- 上限：`MAX_FRAME_BYTES = 4 MiB`、`MAX_PAGE_ENTRIES = 1000`；
- `progress` 帧（`kind=3`）承载「我对端读到哪了」，内核靠它裁剪变更日志；
  因此**progress 帧必须和变更帧一样经过中继**，否则两端日志都无法裁剪。

## 3. 数据模型（sqlite，WAL 单文件）

```sql
CREATE TABLE frames (
  db_id      TEXT NOT NULL,
  seq        INTEGER NOT NULL,      -- 每 db 单调递增，由中继分配
  sender     TEXT NOT NULL,         -- 取自帧头，与令牌绑定的 device 校验
  kind       INTEGER NOT NULL,      -- 0..3，仅用于统计与客户端快速分派
  blob       BLOB NOT NULL,
  created_at INTEGER NOT NULL,
  idem_key   TEXT,                  -- 客户端幂等键，24h 内唯一
  PRIMARY KEY (db_id, seq)
);
CREATE UNIQUE INDEX frames_idem ON frames(db_id, idem_key) WHERE idem_key IS NOT NULL;

CREATE TABLE devices (
  db_id        TEXT NOT NULL,
  device_id    TEXT NOT NULL,
  token_hash   TEXT NOT NULL UNIQUE,   -- SHA-256(token)
  acked_cursor INTEGER NOT NULL DEFAULT 0,
  created_at   INTEGER NOT NULL,
  last_seen    INTEGER,
  revoked_at   INTEGER,
  PRIMARY KEY (db_id, device_id)
);

CREATE TABLE db_state (
  db_id          TEXT PRIMARY KEY,
  next_seq       INTEGER NOT NULL,
  truncated_from INTEGER NOT NULL DEFAULT 0,  -- 已淘汰的起点，客户端据此判断需全量重同步
  bytes          INTEGER NOT NULL DEFAULT 0,
  quota_bytes    INTEGER NOT NULL
);
```

## 4. API v1

所有请求：`Authorization: Bearer <token>`（`/healthz` 与 `enroll` 除外）。

| 方法 | 路径 | 作用 |
|---|---|---|
| `POST` | `/v1/frames` | 上传 1 帧（body = 原始帧字节）；必带 `Idempotency-Key`；返回 `{"seq":N}` |
| `GET` | `/v1/frames?cursor=&limit=` | 拉取 `seq > cursor` 的帧，默认排除 `sender == 自己`；返回 `{"frames":[{"seq","kind","blob_b64"}],"next_cursor","has_more","truncated_from"}` |
| `POST` | `/v1/ack` | body `{"cursor":N}`；只前进不回退 |
| `GET` | `/v1/info` | `{"db_id","acked_cursor","pending_frames","pending_bytes","quota_bytes","frame_ttl_secs","truncated_from","device_id","relay_version"}` |
| `POST` | `/v1/devices` | 签发一次性入网码 `{"enroll_code","expires_at"}`（TTL 10 分钟，一次性） |
| `POST` | `/v1/devices/enroll` | body `{"enroll_code","device_id"}` → `{"token","db_id","device_id"}` |
| `POST` | `/v1/devices/revoke` | body `{"device_id"}`；撤销后该设备令牌立即失效（`401`） |
| `GET` | `/healthz` | `ok`，无任何库信息 |

### 校验规则（拒绝时返回 4xx + 错误码，不泄露帧内容）

1. 令牌有效且未撤销 → 得到 `(db_id, device_id)`；
2. 帧长 ≤ `4 MiB + 4`（长度前缀）→ 否则 `413`；
3. `magic == "SHY2"`、`format == 2` → 否则 `400 invalid_frame`；
4. 帧头 `db_id` 与令牌绑定的一致 → 否则 `403 db_mismatch`；
5. 帧头 `sender` 与令牌绑定的 `device_id` 一致 → 否则 `403 sender_mismatch`（防冒名推送）；
6. `kind ∈ {0,1,2,3}` → 否则 `400`。

## 5. 游标、ack 与 GC

- `seq` 由中继分配（每 `db_id` 单调），与内核的 `after` 是两套东西：前者是传输顺序，后者是库内日志位置。
- `ack` 记录每设备读到哪里；**不立即删帧**（其它设备可能没读）。
- 删除条件（任一）：① 所有活跃设备 `acked_cursor >= seq`；② 超 `frame_ttl`（默认 30 天）；
  ③ 该 `db_id` 超 `quota_bytes`（默认 100 MB）时从最旧开始淘汰。
- 淘汰推进 `truncated_from`；客户端发现自己的 `cursor < truncated_from` 时**不再增量**，
  改走内核的全量重同步（`snapshot_*`，`SyncResyncRequired`）。

## 6. 并发、幂等与限流

- 同一 `db_id` 写入串行（sqlite 事务），保证 `seq` 严格递增且无空洞；
- `Idempotency-Key` 24h 内去重：重试返回原 `seq`，不产生重复帧；
- 限流：每令牌 60 请求/分钟、并发 4；超限 `429` + `Retry-After`；
- 中继只保证「每设备自身顺序 + 全局 `seq` 单调」，不保证跨设备因果序（内核不需要）。

## 7. TLS 与卫生

- 公网部署**必须** TLS：前置 Caddy/nginx（推荐，ACME 自动续期）或内置 rustls；禁止明文 HTTP 监听。
- 最低 TLS 1.2（优先 1.3）；HSTS `max-age=15552000`。
- 令牌只走 `Authorization` 头，禁止 URL 参数（防日志/Referer 泄露）。
- 日志：不记令牌、不记帧内容；IP 只保留 7 天。
- 中继不签发任何证书、不做证书固定（pinning 由客户端决定，当前结论是不做）。

## 8. 部署与运维

```toml
# relay.toml
listen   = "127.0.0.1:8080"
data     = "/var/lib/shio-relay/relay.db"
frame_ttl_secs = 2592000        # 30 天
db_quota_bytes = 104857600      # 100 MB
rate_limit_per_min = 60
```

```bash
relay serve --config relay.toml
relay token issue --db <db_id> --device <device_id>   # 首个设备的令牌
relay token revoke --db <db_id> --device <device_id>
relay stats --db <db_id>            # seq 范围、帧数、字节数、设备 ack 表
relay gc --dry-run|--now            # 按 5 的规则清理
```

- 单实例 + systemd；不做集群、不做多租户控制台。
- 备份可选：`relay.db` 可丢——丢了之后任一设备用快照重新发布即可（见 9）。

## 9. 失败与恢复

| 情形 | 处理 |
|---|---|
| 客户端超时/`5xx` | 用同一 `Idempotency-Key` 重试（指数退避，最多 5 次） |
| 令牌撤销/换中继 | `401` → 客户端提示「需要重新配对」，不静默失败 |
| `truncated_from > cursor` | 客户端全量重同步（`snapshot_*`），成功后重置 ack |
| 中继整库丢失 | 同上：任一设备发布快照，其余设备重同步 |
| 磁盘满 | 先按 GC 淘汰最旧帧；仍写不下返回 `507` + `/v1/info` 报警 |

**中继不是备份**：帧是传输中间态，随时可丢；真正的副本在各设备的库文件里。

## 10. 多应用

- 每个应用的每个库都是独立 `db_id`，中继不区分应用类型；
- 令牌、配额、GC 都按 `db_id` 计；
- 一个应用若要同步多个库，客户端自己管多个 `db_id` 与多个令牌，中继无感。

## 11. 非目标

- 不做服务端解密、搜索、去重、合并；
- 不做账号体系、推送通道、设备发现（局域网直连另行讨论，当前不做）；
- 不做 WebSocket/长连接（HTTP 拉取足够；将来需要再谈可选的 SSE/WS 长轮询）；
- 不做 HA/分片。

## 12. 测试计划

- 单元：帧头校验（magic/format/db_id/sender/kind）、长度上限、幂等去重、seq 单调、GC 规则。
- 集成：两个模拟客户端推/拉/ack；`progress` 帧与裁剪；淘汰后触发 `truncated_from`。
- 端到端：Android 客户端 ↔ 中继 ↔ 第二台设备（真机）跑 changes/apply 往返。
