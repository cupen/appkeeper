## Why

`ak run` 是「跑一条临时命令」的入口，但当前未给 `--app` 时应用名直接取命令 stem，
一旦同名 app 已存在（哪怕只是上次运行留下的记录），第二次 `ak run -- npx ...`
就会收到 409 冲突，用户被迫先想一个不重复的名字或先 `ak remove`。这违背了
ad-hoc 形态「不需要预先规划应用名」的定位：应用名只是注册表的内部标识，不该由
用户承担排重成本。

## What Changes

- **默认自动命名（无 `--app` 时）**：基名仍取命令首 token 的文件名 stem；基名空闲
  则直接使用，已被占用则自动追加单调递增后缀（`npx` → `npx-1` → `npx-2` …），
  保证最终应用名/程序名全局唯一。**BREAKING**：未给 `--app` 的同名重复提交不再
  返回 409，而是自动注册为新名字（依赖 409 的脚本须显式写 `--app`）。
- **占用判定范围**：任一注册记录（`app_dir` 顶层或 `adhoc/`）、内存中的同名应用、
  其他应用已注册的同名程序、以及暂存未落盘的 app，均视为占用并顺延后缀。
- **残留态优先于顺延**：候选名恰好是「已启动但未落盘」的暂存 app 时，仍走既有
  残留确认流程（提交或放弃），不静默换成下一个后缀另起一个进程。
- **显式 `--app` 语义不变**：同名仍按「未带 `--force` 冲突、带 `--force` 覆盖」
  处理；`--force` 且未给 `--app` 时按基名覆盖，不参与自动排重。
- **非法/保留基名**：stem 不是合法标识符或为保留字 `all` 时仍以退出码 2 报错并
  提示用 `--app` 指定（自动排版只负责「重名」，不负责「改名」）。
- **输出**：人类可读输出与 `--json` 的终端结果回传最终选定的名字；发生后缀顺延时
  额外给出自动命名提示（JSON 增加 `auto_named: true`）。
- 命名判定全部在守护进程内完成（命令队列串行，无客户端预检竞态）。

## Capabilities

### New Capabilities

（无）

### Modified Capabilities

- `app-registry`: `ak run` ad-hoc 的缺省命名规则由「取 stem、同名拒绝」改为
  「取 stem、同名自动递增后缀」；明确占用判定范围、残留态优先、`--force` 与显式
  `--app` 的边界，并同步 `--force` 场景下的命名前提。
- `control-plane`: 注册并启动端点的名称校验/同名冲突语义调整为「缺省名解析 +
  自动排重」，回传结果增加「是否自动顺延」字段；显式 `--app` 的冲突行为不变。

## Impact

- 代码：`src/supervisor.rs`（`resolve_adhoc_name` / `adhoc_exists` / `AdhocUpResult`
  与残留判定）、`src/server.rs`（`parse_adhoc_spec` 与 NDJSON 结果帧）、
  `src/main.rs`（`run` 的 `--app` 帮助文案与终端渲染）、`xtask/src/e2e.rs`
  （run 场景断言）。
- 文档：`README.md` 的 `ak run` 用法与命名说明。
- 兼容性：默认行为变化（见 BREAKING）；显式 `--app` 的调用完全不受影响。
- 存储：命名仍落在 `app_dir/adhoc/<name>.toml`，注册表结构与 `ak ps` 展示不变。
