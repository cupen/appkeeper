## 1. 名称解析（supervisor）

- [ ] 1.1 抽出候选名三分态谓词（`Free` / `Taken` / `Staged`）：`Taken` 覆盖 `adhoc_exists` 的既有判定（`app_dir` 顶层与 `adhoc/` 记录、悬空链接、内存 `apps`、同名程序）并并入原 `resolve_adhoc_name` 的「其他应用同名程序」校验，`Staged` 查 `state.staged`；用表驱动单测覆盖每类占用来验证谓词
- [ ] 1.2 扩展 `resolve_adhoc_name` 的缺省名解析：基名空闲直接用；被占用时按现存 `base-<k>` 的最大 k + 1 生成 `base-<N>`（无后缀记录时为 1），跳过 `Taken`、遇 `Staged` 立即返回该候选；显式 `--app` 与 `--force`（未给 `--app`）分支保持原语义。单测覆盖：空闲、占用→`npx-1`、存在 `npx-2` 且有 `npx-1` 空号→`npx-3`、staged→残留、显式名不变、`--force` 用基名
- [ ] 1.3 为 `AdhocUpResult` 增加 `auto_named: bool` 并更新成功 / `unchanged` / `residual` / `register_only` 全部构造点；单测断言顺延为 true、基名命中与显式名为 false

## 2. 控制面端点

- [ ] 2.1 `/v1/run` 的终端 NDJSON 帧与 `--json` 结果携带 `auto_named`（未顺延时为 false）；在 `server.rs` 加端点测试，验证缺省名被占用时回传 `<stem>-1` 与 `auto_named: true`、显式名冲突仍回 409 且不改名
- [ ] 2.2 确认 `parse_adhoc_spec` 的 `name: None` 原样落到守护进程解析（无客户端预检、无额外写盘）；用现有运行冲突测试回归验证冲突帧与残留帧未受影响

## 3. CLI 与输出

- [ ] 3.1 更新 `ak run` 的帮助文案：`--app` 说明缺省取命令 stem 并在冲突时自动追加 `-N`；trailing `--app` 的说明同步；`cargo run -- run --help` 人工核对
- [ ] 3.2 人类可读终端输出在 `auto_named` 为 true 时追加自动命名提示（含基名与最终名），`--json` 保留字段而不打提示；用 `ak run` 连续两次同名命令做手工验证
- [ ] 3.3 把 `resolve_adhoc_name` 里「pass --name」等旧文案改为 `--app`（含无法从命令路径推导名字的分支）；单测断言错误文案包含 `--app`

## 4. 文档与验收

- [ ] 4.1 更新 `README.md` 的 `ak run` 用法与命名说明：缺省自动命名、`npx`/`npx-1` 示例、显式 `--app` 保留冲突语义、`--force` 不参与排重；通读改动段落确认与 spec 一致
- [ ] 4.2 在 `xtask/src/e2e.rs` 增加 run 自动命名场景：连续两次缺省 run 得到 `<stem>` 与 `<stem>-1`、显式 `--app` 冲突仍非零退出、`--force` 无 `--app` 覆盖基名、JSON 含 `auto_named`；`cargo run -p xtask -- e2e --no-browser` 全绿
- [ ] 4.3 全量回归：`cargo test --workspace` 与 `cargo build --release` 通过；`cargo run -- validate` 对注册表无新增告警
