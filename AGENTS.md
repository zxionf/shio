# AGENTS.md

本文档不允许ai修改  

shio

本项目的任务体系由 `sele`（0.21.3）管理：**Plan → Line → Task → Step**。

## 入口

1. `.sele/CONTRACT.md`：不可违反清单，优先阅读。
2. `.sele/` 只通过 `sele` 命令读写；不要手改。
3. 状态：`sele status`、`sele next`、`sele task`、`sele step`、`sele agent`。
4. 操作手册：`docs/sele-0.21.3-agent.md`（项目内存在时）。

## 最小闭环

```bash
sele init

sele plan new <plan> --goal "..." --acceptance "..." --lines <line>
sele plan submit <plan>
sele plan approve <plan> --by human:<name>

sele line new <line> --plan <plan> --goal "..." --acceptance "..." --modules a,b
sele line switch <line> --plan <plan>

sele task add "<title>" --line <line> --acceptance "..."
sele task start T1 --line <line>
sele step add T1 "<title>" --line <line>
sele step start T1 S1 --line <line>
sele step done T1 S1 --line <line>

sele task verify T1 A1 --line <line> --by human:<name>
sele task done T1 --line <line>
sele line verify <line> A1 --plan <plan> --by human:<name>
sele line done <line> --plan <plan>
sele plan verify <plan> PA1 --by human:<name>
sele plan done <plan>
```

## 规则

1. 只经 `sele` 读写 `.sele/`；`log.jsonl` 是 append-only。
2. 显式迁移；I1（全局单 `in_progress` task）、I2（单 task 单 `in_progress` step）。
3. git 只读（`status`/`diff`/`log`/`show`）；`sele git` 写命令执行前先报命令与影响，获用户授权后才执行。
4. 提交频率：一个 task 最多一次，在 task 完成时提交；小修合并。
5. `task done` 前 step 全 `done`/`skipped`；`line done` / `plan done` 有各自前置。
6. 验收只记账，不伪造 `--by`。
7. agent 不改 plan/line 定义、不创建 task；需要变更时用 `sele decision record`
   并请求用户。
8. 不做回滚；不迁移不支持的 `.sele` 结构。
9. 禁止 python/nodejs 脚本；改文件用 `edit`/`write`；校验脚本经 stdin 就地执行。
10. 验证：`sele check` 通过。

## 常用

```bash
sele task / step / next / status / tree / log / check
sele task --all / --global
sele step --all / --global
sele agent
sele git status / diff / log / commit / stash / branch / health / reconcile
sele cache rebuild
```
