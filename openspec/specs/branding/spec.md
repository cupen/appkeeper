# branding Specification

## Purpose

定义项目对外可见标识的命名契约：全名 `appkeeper` 与简称 `ak` 各自的使用面，
使 CLI、环境变量、Web 控制台、配置与部署路径、包与仓库名在跨工件时保持一致，
并保证旧名不在产品面残留、不引入兼容别名。

## Requirements

### Requirement: 全名与简称的分工

项目 SHALL 同时使用全名 `appkeeper` 与简称 `ak`，且二者的使用面 SHALL 固定如下，
不得互换：

- **`ak`（紧凑标识）** 用于：编译产物与 CLI 命令名、clap 命令名、`--help`/
  用法/错误信息/交互提示符中的程序名、环境变量前缀、Web 控制台的 CSS 自定义
  属性与自定义元素标签、systemd unit 名、以及仅用于机器识别的占位/临时标识。
- **`appkeeper`（完整名）** 用于：cargo 包名、Web 控制台页面标题中的产品名、
  app 配置默认文件名、daemon 配置默认路径、默认日志目录、systemd unit 的
  Description、Ansible role 目录名与变量前缀、文档与规范正文中的产品名、
  仓库名。

用户在任何面向用户的输出中看到的程序名 SHALL 为 `ak`；任何文档正文中首次
指代本产品时 SHALL 使用 `appkeeper`。

仅用于机器识别的临时标识（测试、验收工具与基准测量的临时目录、套接字、
workspace 等）SHALL 采用统一前缀模式，不得自造散名：仓库测试
`ak-test-<场景>-<pid>`、验收工具 `ak-xtask-<场景>`、基准测量
`ak-bench-<序号>`；其中基准测量的 workspace 前缀同时是残留清理断言的
扫描契约。

#### Scenario: CLI 自称 ak

- **WHEN** 用户执行 `ak --help`、`ak --version`，或任一子命令报错
- **THEN** 用法/版本/错误信息中的程序名均为 `ak`，不出现 `appkeeper` 或
  旧名

#### Scenario: 文档正文用全名

- **WHEN** 阅读 `README.md` 或 `AGENTS.md` 的产品介绍与术语表
- **THEN** 产品名写作 `appkeeper`，命令示例写作 `ak <子命令>`

#### Scenario: 环境变量前缀为 AK_

- **WHEN** 设置 `AK_WEBUI_BUILD=force` 后构建，或查阅构建文档
- **THEN** 构建系统识别该变量；文档记载的环境变量均为 `AK_*` 前缀

#### Scenario: 测试临时标识统一前缀

- **WHEN** 运行仓库测试、xtask 验收或基准测量，检视其创建的临时目录、
  套接字与 workspace 命名
- **THEN** 标识分别以 `ak-test-`、`ak-xtask-`、`ak-bench-` 开头，不存在
  其他自造前缀

### Requirement: 标识面完整改名

仓库中所有产品标识 SHALL 使用新名（见「全名与简称的分工」），覆盖面 MUST 包含：

- CLI 与二进制：cargo package 名、`[[bin]]` 产物名、clap `name`、帮助与错误
  文本、交互 shell 提示符；
- 环境变量：构建期与运行期变量一律 `AK_*` 前缀；
- Web 控制台：CSS 自定义属性 `--ak-*`、自定义元素标签 `ak-*`、页面标题、
  页面可见标识字样、localStorage 键 `ak:theme` / `ak.refresh_ms`；
- 配置与部署：app 配置默认文件名、daemon 配置默认路径、默认日志目录、
  systemd unit 文件名与 `ExecStart`、Ansible role 目录与变量、示例配置与
  脚手架生成内容中的提示文本；
- 包与仓库：bench crate 目录与 crate 名、webui 包名、GitHub 仓库引用
  （下载链接与发布校验脚本中的仓库名）。

#### Scenario: 产物名为 ak

- **WHEN** 执行 release 构建
- **THEN** 产物为 `target/release/ak`（Windows `ak.exe`），不存在任何
  旧名形态的产物

#### Scenario: 新机器空启动使用新默认路径

- **WHEN** 在无任何配置文件的机器上执行 `ak run`
- **THEN** daemon 以默认设置启动，其默认配置路径为 Linux
  `/etc/appkeeper/daemon.toml`、Windows `%APPDATA%\appkeeper\daemon.toml`，
  默认日志目录为 `/tmp/appkeeper/logs`

#### Scenario: 控制台标签与存储键更名

- **WHEN** 打开内嵌 Web 控制台
- **THEN** 页面标题为 `ak console`，自定义元素标签为 `ak-*`，CSS 自定义属性
  为 `--ak-*`，页面可见标识字样为 `ak`，主题与偏好写入 `ak:theme` /
  `ak.refresh_ms`

#### Scenario: systemd unit 更名

- **WHEN** 用户以 root 执行 `ak daemon install`
- **THEN** 生成 `/etc/systemd/system/ak.service`，其 `ExecStart` 指向当前
  `ak` 可执行文件，`Description` 使用全名 `appkeeper`

### Requirement: 旧名不残留且无兼容别名

改名后 SHALL NOT 保留任何旧名（全名与其紧凑缩写、任意大小写；字面值以归档
变更 `2026-10-14-rename-to-appkeeper` 与 git 历史为权威指派，本规范统称
「旧名」）的兼容面：MUST NOT 提供旧命令名别名或 shim、旧环境变量名回退
读取、旧配置文件名的双读路径、或旧 unit 名的兼容处理。仓库内旧名的残留
SHALL 仅限 `openspec/changes/**`（归档与进行中的变更工件）与 git 历史。
构建、测试与文档校验 MUST NOT 依赖任何旧名标识。

#### Scenario: 旧环境变量不再生效

- **WHEN** 仅设置 `AK_WEBUI_BUILD` 的旧名等价变量（前缀换回旧名，且未设置
  `AK_WEBUI_BUILD`）
- **THEN** 构建系统不识别该变量，按未设置处理

#### Scenario: 旧配置文件不再被识别

- **WHEN** 部署目录中只有旧名形态的 app 配置文件（无 `appkeeper.toml`）
  并执行 `ak compose .`
- **THEN** 命令以非零码失败并提示未找到 `appkeeper.toml`，不静默回退读取
  旧文件名

#### Scenario: 产品面零残留

- **WHEN** 对仓库搜索旧名全名与其紧凑缩写（大小写不敏感、词边界完整；
  排除 `openspec/changes/`、`.git/`、`webui/node_modules/` 与 `target/`）
- **THEN** 无任何命中
