## ADDED Requirements

### Requirement: run ad-hoc 缺省名自动排重

`ak run` 的 ad-hoc 形态在**未显式给出 `--app`** 时 SHALL 由守护进程在命令处理路径内
解析出唯一应用名，用户不必预先规划名字；显式 `--app` 不参与本要求（同名拦截与
`--force` 语义不变）。

基名 SHALL 取命令首 token 的文件名 stem。解析规则：

- 基名未被任何既有名字占用：SHALL 直接以基名为应用名与程序名；
- 基名已被占用：SHALL 依次使用 `<基名>-<N>`，N 从 1 起单调递增。N 的取值 SHALL 为
  现有 `<基名>-<k>` 名字中最大的 k 加一（不存在时取 1），MUST NOT 填充中间空号，
  以免新运行的日志追加到已删除同名前序运行的日志文件；
- 「占用」SHALL 覆盖：`app_dir` 顶层与 `app_dir/adhoc/` 下的注册记录（含悬空链接）、
  内存中的同名应用、**其他应用**已注册的同名程序、以及暂存未落盘的 app；
- 暂存 app 同样不可用，但命中方式不同：候选名恰好对应一个「已启动但未落盘」的暂存
  app 时，SHALL 走既有残留确认流程，MUST NOT 顺延到下一个后缀另起进程（不得把半完成
  的注册静默藏起来）。

推导出的基名不是合法标识符或等于保留字 `all` 时，SHALL 以退出码 2 拒绝并提示用
`--app` 指定名字；自动排重只处理重名，MUST NOT 清洗或改写用户命令推导出的名字。

最终选定的名字 SHALL 在启动判定前确定，并同时作为注册记录名与程序名（ad-hoc 为
单程序 app）；判定 MUST NOT 仅由客户端预检，须在守护进程命令队列的串行执行中完成，
以免与巡检/apply 竞态产生重名。成功结果 SHALL 回传最终选定的名字；发生顺延时
人类可读输出 SHALL 提示该名字为自动命名，`--json` 结果 SHALL 标记 `auto_named: true`。

#### Scenario: 基名空闲时直接使用

- **WHEN** 注册表中不存在 `npx`，执行 `ak run -- npx create-vite app`
- **THEN** 应用名与程序名均为 `npx`，人类输出与 `--json` 结果中的名字为 `npx`，不标记自动顺延

#### Scenario: 基名被占用时顺延为 npx-1

- **WHEN** 已注册 `npx`（无论其程序是否在运行），再次执行 `ak run -- npx create-vite app2`
- **THEN** 守护进程以 `npx-1` 注册并启动新进程，既有 `npx` 的记录与进程保持不变，输出提示该名字为自动命名且 `auto_named: true`

#### Scenario: 再次执行严格递增不复用空号

- **WHEN** 已存在 `npx` 与 `npx-2`（`npx-1` 已被 `ak remove`），执行 `ak run -- npx ...`
- **THEN** 新应用名为 `npx-3`，MUST NOT 复用 `npx-1`

#### Scenario: 顶层 compose 记录同样计入占用

- **WHEN** `app_dir` 顶层已存在 compose 注册的 `node`，执行 `ak run -- node server.js`
- **THEN** 新应用名为 `node-1`，顶层 `node` 记录与进程不受影响

#### Scenario: 与其他应用程序名冲突时顺延

- **WHEN** 名字 `worker` 已被另一个应用注册为程序名，执行 `ak run -- /opt/worker/worker`
- **THEN** 新应用名为 `worker-1`，不返回冲突错误

#### Scenario: 暂存残留优先于顺延

- **WHEN** 上一次 `ak run -- npx ...` 已启动进程但落盘失败（暂存未提交），注册记录不存在，用户再次执行相同命令
- **THEN** 守护进程回传残留态并进入补落盘确认流程，MUST NOT 自动改名为 `npx-1` 再启动一个进程

#### Scenario: 显式 --app 不自动排重

- **WHEN** 已注册 `web`，执行 `ak run -- python -m http.server 9000 --app web`（未带 `--force`）
- **THEN** 仍按同名拦截回传冲突错误，MUST NOT 自动改名为 `web-1`

#### Scenario: 保留字或非法基名被拒绝

- **WHEN** 命令首 token 的文件名 stem 为 `all` 或含非法字符（如 `my.tool`），且未给 `--app`
- **THEN** 以退出码 2 报错并提示用 `--app` 指定名字，不写盘、不启动进程

## MODIFIED Requirements

### Requirement: run ad-hoc 同名应用拦截

`ak run` 的 ad-hoc 两种形态（`--` trailing 与 `--cmd`）SHALL 在**显式给出 `--app`**
时把同名判定交给守护进程；当应用名对应的注册记录已存在（无论位于 `app_dir` 顶层还是
`app_dir/adhoc/`），或内存中已有同名应用（含已注册但未运行的程序），且**未指定
`--force`** 时，守护进程 SHALL 拒绝本次注册并回传结构化冲突错误，MUST NOT 写盘、
MUST NOT 启动新进程、MUST NOT 停止或重启既有进程。未给 `--app` 的缺省命名 SHALL NOT
适用本要求，而按「run ad-hoc 缺省名自动排重」解析出唯一名字。

拒绝信息 SHALL 明确指出该应用名已存在并给出可操作指引（改用其他 `--app` 名称，或
显式加 `--force` 覆盖）。CLI SHALL 以非零退出码退出。

「已启动但未落盘」的残留态不属本要求：该场景仍按 `app-registry` 的残留确认流程处理，
且 MUST NOT 重启既有进程。

#### Scenario: 同名已注册且运行中被拒绝

- **WHEN** 对已由 `ak run` 注册且正在运行的 `web` 再次执行 `ak run -- python -m http.server 9000 --app web`
- **THEN** 守护进程回传冲突错误，既有 `web` 进程的 PID 与 `app_dir/adhoc/web.toml` 内容均保持不变，CLI 以非零退出码退出

#### Scenario: 同名已注册但未运行也被拒绝

- **WHEN** 对已注册但当前未运行的 `web` 再次执行 `ak run -- python -m http.server 9000 --app web`
- **THEN** 注册被拒绝，`app_dir/adhoc/web.toml` 不被改写，不产生新进程

#### Scenario: --restart=no 同样受拦截

- **WHEN** 对已存在的 `web` 执行 `ak run --cmd "python -m http.server 9000" --app web --restart=no`
- **THEN** 注册被拒绝，不写盘；`--restart=no` 不构成覆盖既有应用的例外

#### Scenario: 参数完全相同也不再生

- **WHEN** 对已注册的 `web` 以完全相同的命令行与参数再次执行 `ak run --app web`
- **THEN** 仍以冲突错误拒绝，MUST NOT 返回「幂等成功」或现有 PID

#### Scenario: 残留态仍走确认流程

- **WHEN** 上一次显式 `--app web` 的 run 已启动同名程序但落盘失败，且注册记录不存在
- **THEN** 守护进程回传残留态而非冲突错误，CLI 询问用户是否仅补落盘；确认后复用已在运行的进程，MUST NOT 重启

### Requirement: run ad-hoc --force 强制覆盖

`ak run --force` SHALL 允许覆盖同名已注册应用。覆盖目标 SHALL 为显式 `--app` 指定的
名字；未给 `--app` 时 SHALL 以命令 stem 为基名直接作为覆盖目标，不参与自动排重
（即不会改写为 `<基名>-<N>`）。覆盖判定 SHALL 在守护进程内完成：把本次命令行渲染出的
完整配置文本与磁盘现有注册记录逐字节比较。覆盖目标 SHALL 以记录位置判定：`--force`
仅可覆盖位于 `app_dir/adhoc/` 的记录（run 自家产物）；同名记录位于 `app_dir` 顶层
（compose 生成文件或外部部署链接，见「注册表分区与来源标注」）时 SHALL 拒绝覆盖，
错误信息提示先 `ak remove <name>` 或改用 `ak compose up`，`--force` 不放行。

- **有差异**：SHALL 在通过全部校验后原子重写注册记录（占位 + rename），随后停止旧的同名程序并按新定义重新拉起；`--restart=no` 时只重写记录、不拉起（旧进程 SHALL 被停止，避免旧定义继续运行）。返回新 PID（`--restart=no` 时为 `null`）。
- **无差异**：MUST NOT 写盘、MUST NOT 停止或重启进程、MUST NOT 拉起新进程；CLI SHALL 仅提示「配置无修改，不做任何动作」并以退出码 0 退出；`--json` 结果 SHALL 标记 `unchanged: true` 并回传现有 PID（若有）。

`--force` SHALL NOT 绕过其他既有关卡：与**其他应用**的程序名冲突仍被拒绝；目标记录不是本工具生成的文件（如指向外部部署配置的链接或手工文件）时仍被拒绝覆盖。

「已启动但未落盘」的残留态 SHALL NOT 被 `--force` 改写：仍按残留确认流程处理。

#### Scenario: 有差异时覆盖并重启

- **WHEN** 对正在运行且已注册的 `web` 执行 `ak run --force -- python -m http.server 9000 --app web`
- **THEN** `app_dir/adhoc/web.toml` 被原子重写为新命令行，旧进程被停止，新进程启动并通过启动判定，CLI 回传新 PID

#### Scenario: 无差异时不做任何动作

- **WHEN** 对正在运行且已注册的 `web` 以完全相同的新旧配置执行 `ak run --force -- python -m http.server 8000 --app web`
- **THEN** `app_dir/adhoc/web.toml` 字节不变、进程 PID 不变、不发生 stop/spawn，CLI 仅提示「配置无修改，不做任何动作」并以 0 退出

#### Scenario: 已注册未运行时有差异

- **WHEN** 对已注册但当前未运行的 `web` 执行带不同参数的 `ak run --force --app web`
- **THEN** 记录被重写并拉起新进程

#### Scenario: 已注册未运行且无差异

- **WHEN** 对已注册但当前未运行的 `web` 以相同配置执行 `ak run --force --app web`
- **THEN** 不写盘、不拉起进程，仅提示「配置无修改，不做任何动作」并以 0 退出

#### Scenario: --restart=no 强制覆盖

- **WHEN** 对正在运行的 `web` 执行带不同参数的 `ak run --force --app web --restart=no`
- **THEN** 记录被重写，旧进程被停止，新定义不被拉起

#### Scenario: 未给 --app 时按基名覆盖

- **WHEN** 已注册 `npx`，执行 `ak run --force -- npx create-vite app2`（未给 `--app`）
- **THEN** 覆盖目标为 `npx`（不生成 `npx-1`），按有差异/无差异规则处理

#### Scenario: 自动命名的产物可被 --force 覆盖

- **WHEN** `npx-1` 由自动顺延注册，执行 `ak run --force --app npx-1 -- npx create-vite app3`
- **THEN** 按 `--force` 覆盖语义重写 `app_dir/adhoc/npx-1.toml` 并重启该程序

#### Scenario: 跨应用程序名冲突不因 --force 放行

- **WHEN** `--force` 的目标应用名与**其他应用**已注册的程序名冲突
- **THEN** 注册仍被拒绝，不写盘、不启动进程

#### Scenario: 顶层 compose 记录不被 run --force 覆盖

- **WHEN** 同名注册记录位于 `app_dir` 顶层（`compose up` 快照或脚手架生成文件），执行 `ak run --force --app <name> …`
- **THEN** 覆盖被拒绝并提示 `ak remove` 或 `ak compose up`，目标文件与进程均不变

#### Scenario: 外部部署配置不被 --force 覆盖

- **WHEN** 同名注册记录是指向外部部署配置的链接或非本工具生成文件
- **THEN** `--force` 覆盖被拒绝并说明原因，目标文件不变

#### Scenario: 强制覆盖失败时的处置

- **WHEN** `--force` 的新定义校验失败，或新进程未通过启动判定
- **THEN** 校验失败时目标文件保持原样且无占位残留；启动判定失败时新进程被停止并以结构化错误回传，已原子落盘的新记录保留（覆盖已生效，不回滚旧记录）
