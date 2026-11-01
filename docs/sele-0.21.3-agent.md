# sele 0.21.3 Agent 操作手册

> 面向执行 agent。接口依据 `docs/sele-f.md`，概念依据 `docs/sele-f-d.md`。
> 命令细节以 `sele <cmd> --help` 为准。**0.21 只支持当前 `.sele` 结构**：结构不匹配
> 会被拒绝（退出码 4）。

## 1. 入口与规则

1. **只通过 `sele` 读写 `.sele/`**：不要手改 `plans/`、`lines/`、`log.jsonl`。
2. 先读 `.sele/CONTRACT.md`（不可违反清单）。
3. 感知命令只读：`sele status`、`sele next`、`sele tree`、`sele log`、`sele agent`。
4. 所有写操作都是显式命令；不存在后台推进。
5. 每次写操作检查退出码（见 §10）。

## 2. 最小闭环

```bash
sele init

sele plan new demo --goal "目标" --acceptance "计划验收" --lines auth
sele plan submit demo
sele plan approve demo --by human:<name>

sele line new auth --plan demo --goal "线目标" --acceptance "线验收" --modules core

sele task add "实现登录" --line auth --acceptance "登录可用"
sele task start T1 --line auth

sele step add T1 "设计 schema" --line auth
sele step start T1 S1 --line auth
sele step done T1 S1 --line auth

sele task verify T1 A1 --line auth --by human:<name>
sele task done T1 --line auth
sele line verify auth A1 --plan demo --by human:<name>
sele line done auth --plan demo
sele plan verify demo PA1 --by human:<name>
sele plan done demo
```

## 3. 四层模型与状态

```
Plan → Line → Task（内嵌于 line.json）→ Step（内嵌于 task）
```

- Task **不是独立文件**：所有 task 存在 `lines/<plan>.<line>.json` 的 `tasks` 数组里。
- `uid`（ULID）用于引用与索引；`id`（`T1`）用于命令。

| 实体 | 状态 |
|---|---|
| Plan | `draft` → `pending_approval` → `active` → `done` → `archived`；`draft/pending_approval/active` → `aborted` |
| Line | `draft` → `active` → `done`；`active ↔ blocked`；`draft/active/blocked` → `abandoned` |
| Task | `todo` → `in_progress` → `done`；`in_progress` → `pending_review/paused/blocked/failed`；`pending_review` → `done/in_progress`；`paused/blocked/failed` → `in_progress`；`done` → `archived` → `todo` |
| Step | `todo` → `in_progress` → `done`；`in_progress` → `pending_review/blocked/failed/skipped`；`todo/in_progress` → `skipped` |

## 4. 命令参考

所有命令支持 `--root <dir>` 与 `--output text|json`。

### 4.1 Plan

| 命令 | 说明 |
|---|---|
| `plan new <id> --goal <g> [--acceptance <desc>]... [--lines a,b] [--actor]` | 创建 draft；acceptance 自动编号 `PA1...` |
| `plan submit <id> [--reason]` | 至少一条 line |
| `plan approve <id> [--by <who>] [--reason]` | 设置 approved_by/at |
| `plan reject <id> --reason <r>` | 回到 draft |
| `plan abort <id> [--reason]` | 进入 aborted |
| `plan verify <id> <PA-id> [--by <who>]` | 验收点置 pass |
| `plan done <id> [--reason]` | 所有 line done/abandoned + 计划验收全 pass |
| `plan archive <id> [--reason]` | 移动 `plans/`、`lines/` 到 `archive/<id>-<ts>/`，写 `_manifest.json` |
| `plan restore <id> [--from <ts>] [--reason]` | 按 manifest 移回，状态回 active |
| `plan switch <id>` | 写 `cache/context.json`（清除 line） |
| `plan status/show/list` | 查询；`list [--status]` |

### 4.2 Line

| 命令 | 说明 |
|---|---|
| `line new <id> --plan <p> --goal <g> --modules a,b [--acceptance <desc>]...` | plan 必须 active；创建为 `active` |
| `line activate <id> --plan <p>` | draft → active |
| `line switch <id> [--plan <p>]` | 写上下文 |
| `line verify <id> <A-id> [--by] --plan <p>` | 验收点置 pass |
| `line done <id> [--reason] --plan <p>` | 所有 task done/archived + 线验收全 pass |
| `line abandon <id> --reason <r> --plan <p>` | 放弃 |
| `line block <id> --reason <r> --plan <p>` / `line unblock <id> --plan <p>` | 阻塞/恢复 |
| `line dep-add <id> --target <t> --plan <p>` / `line dep-remove ...` | 线依赖；拒绝环 |
| `line check [--plan <p>] [--id <line>]` | 依赖环检查 |
| `line status <id> --plan <p>` / `line list [--plan] [--status]` | 查询 |

### 4.3 Task

| 命令 | 说明 |
|---|---|
| `task add "<title>" --line <l> [--plan] [--modules a,b] [--depends <ref>]... [--acceptance <desc>]... [--review-policy strict\|lenient\|mixed]` | `--depends` 支持 `T1` / `line.T1` / `plan.line.T1`，落库为 uid |
| `task insert "<title>" --line <l> --after T1\|--before T1 [--pause-current]` | 相对锚点插入 |
| `task start <id> [--line] [--plan] [--reason]` | depends 全 done/archived；自动暂停当前 in_progress（I1） |
| `task pause <id> --reason <r> [--hint <h>]` | 写 `pause_context`（含 git_snapshot） |
| `task resume <id> [--reason]` | paused → in_progress |
| `task keep <id>` | 重置 skipped_count |
| `task done <id>` | step 全 done/skipped；验收未满足 review_policy → pending_review |
| `task approve <id> [--reason]` / `task reject <id> --reason <r>` | pending_review → done / in_progress |
| `task fail <id> --reason <r>` / `task retry <id>` | failed / 重试 |
| `task block <id> --reason <r>` / `task unblock <id> [--reason]` | blocked / 恢复 |
| `task verify <id> <A-id> [--by]` | 验收点置 pass |
| `task archive <id> [--reason]` / `task revive <id> [--reason]` | 归档/复活（状态变更，task 仍在 line.json） |
| `task move <id> --to-line <l> [--plan]` | 跨线移动，uid 不变 |
| `task update <id> [--modules a,b] [--review-policy]` | 元数据更新 |
| `task locate <id>` | 返回所在 `lines/<plan>.<line>.json` |
| `task status <id>` / `task list [--line] [--plan] [--status] [--all]` | 查询 |

### 4.4 Step

| 命令 | 说明 |
|---|---|
| `step add <task> "<title>" [--after <S>] [--requires-review]` | 追加/插入 |
| `step start <task> <step>` | task 必须 in_progress；同 task 无其他 in_progress（I2） |
| `step done <task> <step>` | `requires_review` → pending_review |
| `step approve/reject <task> <step> [--reason]` | 审核 |
| `step skip <task> <step> --reason <r>` / `step goto <task> <step> --reason <r>` | 跳过/跳转（中间标 skipped） |
| `step fail/block <task> <step> --reason <r>` / `step unblock <task> <step>` | 失败/阻塞/恢复 |
| `step lock <task> <step> --reason <r>` / `step unlock <task> <step>` | 锁后拒绝 start/done/skip/update/delete/move |
| `step update <task> <step> [--title] [--desc] [--requires-review\|--no-requires-review]` | 元数据 |
| `step delete <task> <step> --reason <r>` | 仅 todo/skipped |
| `step move <task> <step> --after <S>` | 仅 todo |
| `step list <task>` | 查询 |

### 4.5 查询

| 命令 | 说明 |
|---|---|
| `task [--all] [--global]` | 当前 line（`--all` 当前 plan，`--global` 全部）的 task，按依赖拓扑序 |
| `step [--all] [--global]` | 当前 task 的 step（`--all` 当前 line，`--global` 全部），按数组顺序 |

> 视图中的状态显示短名：`in_progress` → `active`，`pending_review` → `review`；
> JSON 仍返回内部值（`in_progress` / `pending_review`）。
| `next [--plan] [--line] [--resume]` | 就绪 task（存在 in_progress 时为空），按 created_at/uid |
| `status [--plan] [--with-git]` | plan/line/task 摘要、warnings、git 信息 |
| `tree [--plan] [--all]` | 四层结构 |
| `log [--plan] [--line] [--task] [--step] [--since <ts>] [--decisions] [--replay] [--limit <n>]` | 事件日志 |
| `check [--plan]` | 一致性检查（引用、索引、归档、依赖环、done 一致性） |
| `decision record "<内容>" [--plan] [--line] [--task] [--step]` | 写 `op: decision` |
| `agent [--plan] [--line] [--task]` | 输出约束提示词 |
| `config` | 显示有效配置 |
| `cache rebuild` | 重建 `cache/index.json` |

### 4.6 Git 代理

| 命令 | 说明 |
|---|---|
| `git status [--short]` / `git diff [--stat]` / `git log [--limit]` | 透传 |
| `git commit --task <t> --step <s> --message <m> [--files a,b] [--plan] [--line]` | 校验 task/step in_progress → add → commit → `git.commit` 事件 |
| `git stash --task <t> [--message]` / `git stash pop` | stash 排除 `.sele` |
| `git branch create <name>` / `git branch switch <name>` | 分支 |
| `git health` | HEAD/branch/dirty |
| `git reconcile --sync-to <sha>` | 漂移后承认当前 HEAD |

**Git 只读**：agent 只允许 `git status/diff/log/show` 等查询；上表写命令（`commit`/`stash`/`branch`/`reconcile`）必须先获用户授权。
**没有自动提交**：唯一的 `git commit` 由上面这条命令手动触发，且是用户批准后。

## 5. 迁移前置（必须记住）

| 迁移 | 前置 |
|---|---|
| `plan submit` | 至少一条 line |
| `line new` | plan 为 active |
| `task start` | depends 全 done/archived；全局无其他 in_progress（有则自动 pause） |
| `step start` | task 为 in_progress；同 task 无其他 in_progress（I2） |
| `task done` | 所有 step done/skipped；验收按 review_policy |
| `line done` | 所有 task done/archived；线验收全 pass |
| `plan done` | 所有 line done/abandoned；计划验收全 pass |
| `task archive` | task 为 done |
| `plan archive` | plan 为 done |

## 6. 验收与审核

| review_policy | 完成条件 |
|---|---|
| `strict` | 所有验收点 pass 且 `verified_by` 以 `human` 开头 |
| `lenient` | 所有验收点 pass |
| `mixed`（默认） | `requires_human` 的验收点需 human；其余 pass 即可 |

- `step done` 对 `requires_review` 的 step 转 `pending_review`，需 `step approve`。
- `task done` 未满足 review_policy 时转 `pending_review`，需 `task approve` 或补充 `task verify` 后再 done。
- `pending_review` 会阻塞依赖它的 task（不是 done）。

## 7. 事件与漂移

- 所有迁移写 `log.jsonl`（append-only），`git commit` 写 `git.commit`（`after` = commit sha）。
- 漂移：D1 HEAD 前进（自动更新指纹）、D2 回退（阻断 commit）、D3 分叉（阻断）、D6 分支切换（记录）。
- 漂移阻断时：`git reconcile --sync-to <当前 HEAD>` 承认现实，再继续。
- `git health` 只报告 HEAD/branch/dirty。

## 8. 归档与恢复

- `plan archive` 把 `plans/`、`lines/`（task 已内嵌）移到 `archive/<id>-<ts>/`，写 `_manifest.json`。
- `plan restore [--from <ts>]` 按 manifest 移回。
- `task archive` 只改状态（`archived`），task 仍在 line.json 的 `tasks` 中。
- 归档的 task 不参与 `next`，`task list` 默认隐藏（`--all` 显示）。

## 9. 输出与退出码

- 成功：text 输出人类摘要；json 输出对象。
- 错误（text）：写 stderr，`Error: <message>`；json：`{"error": "...", "suggestion": "...", "next": [...]}`。
- 退出码：

| 码 | 含义 |
|---|---|
| 0 | 成功 |
| 1 | 用法/参数错误 |
| 2 | 状态冲突（非法迁移、依赖未满足、锁、module_scope block） |
| 3 | 一致性检查失败（check） |
| 4 | 环境/数据错误（JSON 损坏、git 失败、漂移阻断、旧 `.sele` 布局） |

## 10. 推荐工作循环

```
1. 感知：sele next / sele status / sele agent
2. 启动：sele task start <id>
3. 推进：sele step start/done，需要时 sele git commit --task --step
4. 反馈：sele task pause / done / fail
5. 记录：sele decision record（遇岔路）
```

- 进入 `in_progress` 必须对应一次退出迁移（pause/done/fail/block）。
- 切换 task 用 `task start <other>`（自动 pause 当前）。
- 长时间离开用 `task pause --reason --hint`。
- **提交时机**：一个 task 最多一次，在 task 完成时；先审查命令与 diff 再执行。

## 11. 常见错误与处理

| 错误 | 处理 |
|---|---|
| `Task id is ambiguous across lines` | 加 `--line`，或 `line.T1` / `plan.line.T1` |
| `Task cannot go from X to Y` | 查状态机；先 resume/retry/approve |
| `Step ... is locked` | `step unlock` |
| `Dependency ... is not done` | 先完成依赖，或检查 uid 引用 |
| `Acceptance not found` | 用 `task status` / `line status` 查验收 id |
| `Working tree is not clean` | 先 commit 或 stash（`sele git stash`） |
| `Git drift D2/D3` | `sele git reconcile --sync-to <HEAD>` |
| `check failed` | 按 problems 逐项修；索引问题用 `cache rebuild` |
| `incompatible .sele layout` | 删除 `.sele/`，重新 `sele init`（不迁移） |

## 12. 禁止项

- 不手改 `.sele/` 下的 JSON 与 `log.jsonl`。
- git 只读；`sele git` 写命令（commit/stash/branch/reconcile）需用户授权。
- 不修改 plan/line 定义、不创建 task（这是人的权限）。
- 不在 `check` 失败时继续推进。
- 不尝试迁移不支持的 `.sele` 结构。
