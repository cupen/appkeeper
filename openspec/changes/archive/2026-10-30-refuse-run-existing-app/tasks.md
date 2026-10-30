## 1. CLI 与线协议：--force

- [x] 1.1 `Cmd::Run` 增加 `--force` 布尔旗标并在帮助文案说明「同名默认拒绝，`--force` 覆盖已注册应用」；`ak run --help` 可见该旗标
- [x] 1.2 `split_adhoc_trailing` 的 `AdhocFlags` 识别 `--force`（`--` 形态与 `--cmd` 形态语义一致）；补 CLI 解析单测，覆盖两形态与「`--force` 出现在 trailing 任意合法位置」
- [x] 1.3 `run_adhoc` 把 `force` 写入 `/v1/run` 请求体；`server::parse_adhoc_spec` 解析为 `AdhocSpec.force`；补请求体序列化/解析单测
- [x] 1.4 CLI 终端帧处理：`unchanged:true` 时人类输出固定为「配置无修改，不做任何动作」并以 0 退出，`--json` 输出结构化对象；补 `render_run_terminal` 单测

## 2. 守护进程：默认拒绝与 --force 覆盖

- [x] 2.1 `AdhocUpError` 新增 `Conflict(String)`（409、phase `conflict`），补 Display/映射单测
- [x] 2.2 `cmd_adhoc_up` 在 residual 检查之后、`render_adhoc` 之前做存在性判定（注册记录在盘 / `st.apps` / 同名程序 `def.app == name`），未带 `force` 时返回 `Conflict` 且不触碰进程与磁盘；单测覆盖「运行中」「已注册未运行」
- [x] 2.3 `force` 且已存在时走独立覆盖路径：先 `render_adhoc` 校验 → `guard_scaffold_overwrite` → 读取现有记录 → 逐字节比较；单测覆盖外部配置被拒绝
- [x] 2.4 无差异分支：返回 `unchanged=true`，断言目标文件字节、mtime、进程 PID 均不变，且没有 spawn/stop 调用；单测覆盖运行中与未运行两种起点
- [x] 2.5 有差异分支：原子重写记录后停旧启新（`--restart=no` 只停不启），返回 `landed=true` 与新 PID；单测断言文件内容、旧进程被停止、新进程被拉起、返回 PID 变化
- [x] 2.6 `--force` 不放行跨应用程序名冲突；单测复用既有冲突夹具断言仍为校验错误
- [x] 2.7 residual 流程不读取 `force`；既有 residual 单测保持通过

## 3. 服务端流式响应

- [x] 3.1 `respond_adhoc_run` 对 `unchanged` 直接回终端帧（`ok:true, unchanged:true, pid`），不进入启动判定、不 commit；单测断言帧序列只有一帧
- [x] 3.2 对 `landed` 跳过 commit，等待启动判定后回终端帧；失败/超时分支停止新程序并回结构化错误；单测覆盖成功、失败、超时
- [x] 3.3 保留 register_only 分支：force + `--restart=no` 有差异时落盘后直接回 `ok:true`（pid 为 null），无差异时回 `unchanged:true`
- [x] 3.4 `cargo test --workspace` 通过，无 dead_code/unused 警告

## 4. e2e：默认拦截

- [x] 4.1 改写 `xtask/src/e2e.rs` R3 再生断言：无 `--force` 的同名再次 run 非零退出，`app_dir/<name>.toml` 字节不变，既有进程 PID 不变且仍存活
- [x] 4.2 改写 RDF9b：无 `--force` 同名再提交被拒绝；新增「已注册未运行」「`--restart=no` 同名」「参数完全相同」三个拦截用例
- [x] 4.3 断言冲突输出包含应用名与 `--force` 指引；失败路径无占位/注册残留

## 5. e2e：--force 覆盖

- [x] 5.1 运行中 + 有差异：`ak run --force -- <新参数>` 退出 0，文件被全量重写，旧 PID 消失/新 PID 生效，`ps` 反映新状态
- [x] 5.2 运行中 + 无差异：退出 0，输出含「配置无修改，不做任何动作」，文件字节与 mtime 不变，PID 不变且进程仍存活
- [x] 5.3 已注册未运行 + 有差异：退出 0，文件重写且程序被拉起
- [x] 5.4 已注册未运行 + 无差异：退出 0，仅提示，文件不变且程序仍不被拉起
- [x] 5.5 `--force --restart=no` 有差异：文件重写、旧进程停止、新定义不拉起；无差异：仅提示
- [x] 5.6 `--force` 与跨应用程序名冲突：仍非零退出、不写盘；`--force` 指向外部部署配置的链接：被拒绝且目标不变
- [x] 5.7 `--force` 新进程启动判定失败：结构化错误回传、新进程被停止、无 `.pending` 残留（新记录保留）
- [x] 5.8 `--json` 形态覆盖 5.1/5.2，断言 `unchanged` 与 `pid` 字段
- [x] 5.9 运行 `cargo run -p xtask -- e2e`（或 `--no-browser` 快跑）全部通过

## 6. 文档与规范

- [x] 6.1 更新 `AGENTS.md`：`run` 同名默认拒绝、`--force` 覆盖、无差异空操作三句话写清
- [x] 6.2 检查 README/命令速查是否提及同名再生，若有则同步
- [x] 6.3 `openspec validate refuse-run-existing-app` 与 `openspec validate --specs` 通过
