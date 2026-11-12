# control-plane Specification Delta

## ADDED Requirements

### Requirement: compose up 端点族

守护进程 SHALL 为 `ak compose up` 提供两段式端点（声明文件语义与撞名分支见 `app-registry` 的「声明式编排（compose up）」）：

- `POST /v1/compose/up/preview`：**无副作用**预览。请求 SHALL 携带声明文件内容与渲染基准目录（基准目录用于把相对路径绝对化），MUST NOT 只传文件路径——TCP 形态下 CLI 与守护进程可不在同一台机器；另携带请求的应用名，以及 `-e/--env` 传入的变量名值对（缺省为空对象）。渲染前 SHALL 按 `app-registry` 的替换规则把变量值代入声明文本（替换发生在解析之前，所有字段同源生效）。守护进程 SHALL 解析、校验并渲染，与现有注册记录比对后回传**分类**与详情：
  - `fresh`：无同名记录，可直接提交；
  - `identical`：渲染结果与注册记录逐字节相同，附各程序当前运行态（供 CLI 呈现 ensure-running 结果）；
  - `needs-update`：同名 compose 记录（顶层）与渲染结果有差异，附差异摘要与既有程序运行态；
  - `adhoc-takeover`：同名记录位于 `app_dir/adhoc/`（run 产物），附其运行态；
  - `external-refused`：同名记录为外部部署链接，附 `ak remove` 指引。
  校验失败（非法名/保留字、解析失败、程序名跨 app 冲突、依赖成环）SHALL 以校验类错误回传，零副作用。
- `POST /v1/compose/up`：执行。请求携带同样的声明内容、应用名、`-e/--env` 变量值对与 `force` 布尔。同名判定 SHALL 在守护进程命令处理路径完成，MUST NOT 仅由客户端预检决定（与「注册并启动端点」同源要求，防与巡检/apply 竞态）。分类为 `needs-update` 或 `adhoc-takeover` 而未带 `force` 时 SHALL 以状态冲突类错误回传并附预览详情（供非交互客户端呈现指引），MUST NOT 写盘、MUST NOT 启停任何进程；`external-refused` 无论 `force` 一律以校验/冲突类错误拒绝回传。

执行 SHALL 复用「注册并启动端点」的 staged 契约并扩展到多程序：按 priority 与 depends_on 拓扑拉起、逐个等待启动判定、**全部通过才落盘**；任一失败 SHALL 由服务端自行中止——停止本轮已拉起的程序、清理全部占位残留——并以结构化错误（失败阶段、程序名、原因）回传。进度 SHALL 按「启动进度流式反馈」语义流式回传（逐程序事件帧）。

`identical` 提交 SHALL 为无写盘操作：未运行的程序按守护启动规则拉起（ensure-running）；全部按策略在运行时回传 `unchanged` 成功。`fresh` 提交即完整注册并启动。`needs-update`/`adhoc-takeover` 带 `force` 的提交：停止同名 app 既有程序（收编分支先删除 `adhoc/` 记录）→ 原子落盘新记录 → 按声明拉起。

#### Scenario: 预览返回 fresh

- **WHEN** 客户端对未注册名提交声明内容至 `/v1/compose/up/preview`
- **THEN** 回传 `fresh` 分类与渲染出的程序清单，无任何写盘或进程变化

#### Scenario: 预览返回 needs-update 附差异摘要

- **WHEN** 同名顶层记录与渲染结果有差异时请求预览
- **THEN** 回传 `needs-update`、逐程序差异摘要与既有程序运行态，零副作用

#### Scenario: 预览返回 external-refused

- **WHEN** 同名记录为外部部署链接时请求预览
- **THEN** 回传 `external-refused` 与 `ak remove` 指引，零副作用

#### Scenario: 需交互分类未带 force 被拒绝

- **WHEN** 分类为 `needs-update` 或 `adhoc-takeover` 的提交未携带 `force`
- **THEN** 以状态冲突类错误回传并附预览详情，不写盘、不启停进程

#### Scenario: force 更新停旧写新拉起

- **WHEN** 对 `needs-update` 提交携带 `force=true`
- **THEN** 既有程序被停止、注册记录被原子重写、新声明按序拉起并逐程序回传结果

#### Scenario: 收编删除 adhoc 记录

- **WHEN** 对 `adhoc-takeover` 提交携带 `force=true`
- **THEN** 该程序被停止、`app_dir/adhoc/<name>.toml` 被删除、新记录落盘顶层并按声明拉起

#### Scenario: identical 提交无操作或补拉起

- **WHEN** 对 `identical` 提交且部分程序未运行（无用户停止记录）
- **THEN** 未运行的程序被拉起、注册记录不被改写；全部在运行时回传 `unchanged` 成功

#### Scenario: 启动失败服务端中止

- **WHEN** 提交后某程序未通过启动判定
- **THEN** 本轮已拉起的程序被停止、无注册文件与占位残留，回传含失败阶段与程序名的结构化错误

#### Scenario: 逐程序流式进度

- **WHEN** 多程序声明提交执行
- **THEN** 守护进程按启动顺序流式回传逐程序进度帧（启动判定通过/失败），最终回传整体结果

#### Scenario: 远程客户端按内容提交

- **WHEN** TCP 形态控制端点下，CLI 从另一台机器提交声明内容与基准目录
- **THEN** 渲染与注册成功，全程不依赖守护进程侧存在 `.appkeeper.toml` 文件路径
