# Proposal: rename-to-appkeeper

## Why

项目名 `xkeeper` 与产品实际形态已经脱节：它守护的是任意"应用/程序"
（app/program），`x` 前缀没有实义，也与仓库既有的 `app` / `app_dir` /
`app-default` 语汇不在同一套词上。更名 `appkeeper`（简称 `ak`）把包名、
二进制名、配置文件名与部署路径统一到一个自解释的名字下，并把「全名 / 简称」
的分工固定成契约，避免再出现 frontend→webui 那类局部改名后标识散落各处、
没人说得清哪一处该用哪个名字的问题。

## What Changes

- **BREAKING**：编译产物与 CLI 命令名 `xkeeper` → **`ak`**（clap `name`、
  `--help`/用法/错误信息/交互提示符、`service install` 写入的 `ExecStart`）。
- **BREAKING**：环境变量前缀 `XKEEPER_*` → `AK_*`（`XKEEPER_WEBUI_BUILD` →
  `AK_WEBUI_BUILD`、`XKEEPER_WEBUI_DEV_BACKEND` → `AK_WEBUI_DEV_BACKEND` 等）。
- **BREAKING**：app 配置默认文件名 `xkeeper.toml` → `appkeeper.toml`；
  daemon 配置默认路径 `/etc/xkeeper/daemon.toml` →
  `/etc/appkeeper/daemon.toml`（Windows `%APPDATA%\xkeeper\` →
  `%APPDATA%\appkeeper\`）；默认日志目录 `/tmp/xkeeper/logs` →
  `/tmp/appkeeper/logs`。
- **BREAKING**：systemd unit `xkeeper.service` → `ak.service`；Ansible role
  `.ansible/roles/xkeeper` → `.ansible/roles/appkeeper`、变量前缀 `xkeeper_*`
  → `appkeeper_*`、目标机二进制落位 `/usr/local/bin/xkeeper` →
  `/usr/local/bin/ak`、daemon 配置落位 `/etc/xkeeper/daemon.toml` →
  `/etc/appkeeper/daemon.toml`。
- **BREAKING**：cargo 包 `xkeeper` → `appkeeper`；bench crate `xkeeper-bench`
  → `appkeeper-bench`（目录同步改名，workspace 成员与 `-p` 调用跟进）；
  webui 包名 `xkeeper-webui` → `appkeeper-webui`。
- Web 控制台标识：CSS 自定义属性 `--xkeeper-*` → `--ak-*`；自定义元素标签
  `xkeeper-*` → `ak-*`；页面标题 `xkeeper console` → `ak console`；
  localStorage 键 `xkeeper:theme` / `xkeeper.refresh_ms` → `ak:theme` /
  `ak.refresh_ms`。
- 运行期杂项标识：构建占位文件 `.xkeeper-placeholder` → `.ak-placeholder`；
  webui 临时目录前缀、bench 临时 app 前缀 `xkeeper-bench-` → `ak-bench-`。
- 文档与规范：`README.md`、`AGENTS.md`、`.ansible/README.md`、`conf/`、
  `examples/` 与 `openspec/specs/**` 中的产品名按分层规则统一书写。规范文本
  中的产品名是**符号引用**，随命名契约一起更新，不构成 requirement 语义变更。
- **无兼容层**：不提供旧名别名、符号链接或双读路径。旧名残留仅限
  `openspec/changes/archive/**` 的历史变更记录与 git 历史。

## Capabilities

### New Capabilities

- `branding`: 项目命名与标识分层契约——全名 `appkeeper` 与简称 `ak` 的分工，
  各标识面（CLI/二进制、环境变量、Web 控制台、配置与部署路径、包与仓库）
  应使用的形式，以及旧名不得残留、不得留兼容别名的约束。

### Modified Capabilities

（无。）本次是标识改名，不改变任何既有能力的行为契约。`configuration`、
`service-registration`、`ansible-deployment`、`control-plane` 等规范中出现的
产品名与路径，是命名契约所辖的符号引用，按 `branding` 的分层规则统一改写即可；
其 requirement 的语义（做什么、什么条件下发生什么）保持不变，因此不产生
spec delta。

## Impact

- Rust：`Cargo.toml`（package name、workspace members）、`Cargo.lock`、
  `build.rs`（环境变量名、占位文件、crate 引用）、`src/*.rs`（CLI `name` 与
  帮助文本、错误/提示信息、默认路径常量、systemd unit 生成、临时标识）、
  `tests/revamp_integration.rs`。
- 子 crate：`xkeeper-bench/` → `appkeeper-bench/`（目录、crate 名、`BENCH_PREFIX`
  与临时 app 命名、报告字段中的二进制名）、`xtask/`（发现 daemon / bench 产物
  的路径与 `-p` 调用）。
- 前端：`webui/index.html`（标题、元素标签、localStorage 键、预绘制脚本）、
  `webui/src/styles/*.css` 与 `tokens.css`（`--xkeeper-*` → `--ak-*`，含
  `wa-overrides.css`）、`webui/src/**/*.ts`（自定义元素标签、存储键、
  `xkeeper-*` 类名）、`webui/package.json`、`webui/vite.config.ts`。
- 部署与配置样例：`conf/daemon.toml`、`conf/app.toml`、`examples/daemon.toml`、
  `examples/demo-app/xkeeper.toml` → `appkeeper.toml`、`.ansible/**`。
- 文档与规范：`README.md`（约 150 处）、`AGENTS.md`、`.ansible/README.md`、
  `openspec/specs/**`（16 个能力规范）、`LICENSE`。
- 兼容性影响（用户可感知）：命令名、环境变量、app 配置文件名、daemon 配置
  默认路径、systemd unit 名全部变化——已部署的机器需手工迁移配置与 unit
  （本变更不提供自动迁移）；浏览器端主题与偏好因存储键更名而回到默认值。
