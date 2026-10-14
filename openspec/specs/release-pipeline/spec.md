# release-pipeline Specification

## Purpose
规定 appkeeper 二进制的 GitHub Release 发布契约：什么事件触发发布、版本如何与
代码对齐、发布哪些平台产物及其命名与包内容、如何提供 SHA256 校验和与发布说明，
以及发布前必须满足的质量门禁。本能力只约束发布流水线对外可观察的结果，
不约束 workflow YAML 的内部实现方式。

## Requirements

### Requirement: tag 触发与版本一致性

推送形如 `v<version>` 的 git tag SHALL 触发发布流水线。tag 中的版本号 SHALL 与
`Cargo.toml` 的 `version` 字段逐字符一致，不一致时流水线 MUST 失败且 MUST NOT 创建
任何 release 或上传任何产物。流水线 MUST NOT 修改 `Cargo.toml` 的版本字段。

#### Scenario: 版本一致时正常发布

- **WHEN** 推送 tag `v0.1.0` 且 `Cargo.toml` 中 `version = "0.1.0"`
- **THEN** 流水线通过版本校验，进入构建与发布阶段

#### Scenario: 版本不一致时中止

- **WHEN** 推送 tag `v0.2.0` 但 `Cargo.toml` 中 `version = "0.1.0"`
- **THEN** 流水线以失败结束，GitHub 上不存在对应 release，release 资产列表为空

#### Scenario: 非版本 tag 不触发

- **WHEN** 推送不符合 `v*` 形态的 tag（如 `nightly-20261013`）
- **THEN** 发布流水线不运行

### Requirement: 跨平台构建矩阵

流水线 SHALL 为每个目标平台编译 release 版 `ak` 并产出可分发归档，恰好覆盖三个
目标：`x86_64-unknown-linux-musl`（静态链接）、`x86_64-unknown-linux-gnu`、
`x86_64-pc-windows-msvc`。归档命名 SHALL 以 Rust target 三元组消除歧义，形如
`ak-<version>-<target-triple>.<tar.gz|zip>`，其中 Linux 用 `.tar.gz`、Windows 用
`.zip`；每个归档 SHALL 包含对应平台的可执行文件（`ak` 或 `ak.exe`）以及仓库的
`LICENSE` 与 `README.md`。任一目标编译失败时，流水线 MUST 失败且 MUST NOT 发布。

#### Scenario: 三平台产物齐备

- **WHEN** 版本校验通过且三个目标均编译成功
- **THEN** release 上同时存在 `ak-<version>-x86_64-unknown-linux-musl.tar.gz`、
  `ak-<version>-x86_64-unknown-linux-gnu.tar.gz` 与
  `ak-<version>-x86_64-pc-windows-msvc.zip`，每个包解压后含可执行文件、LICENSE、
  README

#### Scenario: musl 产物不依赖 glibc

- **WHEN** 在仅安装了 musl target 的干净 Linux runner 上构建 musl 产物
- **THEN** 产出的二进制为静态链接，可在不含 glibc 的环境（如 alpine）直接运行

#### Scenario: Windows 产物可用

- **WHEN** 取 `ak-<version>-x86_64-pc-windows-msvc.zip` 解压得到 `ak.exe`
- **THEN** 在 Windows 上运行 `ak.exe --version` 能正常输出版本号

#### Scenario: 单目标失败阻断发布

- **WHEN** 三个目标中任意一个编译失败
- **THEN** 流水线失败，GitHub 上不出现该 tag 的 release

### Requirement: SHA256 校验和

流水线 SHALL 为每个发布归档计算 SHA256，并汇总为单个 `SHA256SUMS` 资产上传到
release；其内容 SHALL 为标准 sha256sum 格式（每行 `<hash>  <文件名>`，文件名仅为
归档名）。`SHA256SUMS` 的完整内容 SHALL 同时以代码块形式出现在 release notes 正文中。

#### Scenario: 校验和可被标准工具校验

- **WHEN** 用户下载全部归档与 `SHA256SUMS` 到同一目录并执行 `sha256sum -c SHA256SUMS`
- **THEN** 命令对每个归档输出 `OK` 且退出码为 0

#### Scenario: 校验和内嵌发布说明

- **WHEN** 打开该 release 的说明页面
- **THEN** 正文中的校验和代码块内容与 `SHA256SUMS` 资产逐字节一致

### Requirement: 发布说明内容

release notes SHALL 由自动生成的变更列表（依据 git 历史生成）与流水线追加的资产
清单、校验和段落组成；资产清单 SHALL 逐一列出每个归档及其对应平台与架构。
流水线 MUST NOT 要求人工撰写发布说明。

#### Scenario: 说明包含资产清单

- **WHEN** 打开任意一次已发布 release 的说明页面
- **THEN** 正文包含自动生成的变更列表，以及三份归档的清单（平台、架构、文件名）

#### Scenario: 首次发布无历史对比

- **WHEN** 仓库的第一个 tag 被发布（不存在上一个 release）
- **THEN** 自动生成的变更列表可为空，资产清单与校验和段落仍正常生成

### Requirement: 发布形态与权限

流水线 SHALL 直接创建并公开发布 release，MUST NOT 创建 draft 待人工审核。当 tag
版本号包含 `-rc`、`-beta` 或 `-alpha` 预发布标识时，release SHALL 被标记为
prerelease；否则 SHALL 为正式 release。流水线权限 SHALL 收敛为仅
`contents: write`，并使用仓库自带的 `GITHUB_TOKEN`，MUST NOT 依赖个人访问令牌。

#### Scenario: 正式 tag 直接公开

- **WHEN** 推送 tag `v0.1.0` 且全部门禁通过
- **THEN** release 被创建为非 draft、非 prerelease 的正式发布

#### Scenario: 预发布 tag 标记 prerelease

- **WHEN** 推送 tag `v0.2.0-rc.1`
- **THEN** release 被创建为 prerelease

#### Scenario: 权限收敛到 contents 写

- **WHEN** 检查发布 workflow 的权限声明
- **THEN** 顶层权限为 `contents: write`（其余为 none），凭据来自 `GITHUB_TOKEN`

### Requirement: 发布前质量门禁

发布流水线 SHALL 在编译任何发布产物之前完成质量门禁：在 ubuntu 与 windows 两个
平台各运行一次完整测试，全部通过方可继续。发布构建 MUST 真实构建并嵌入 Web 控制台
（不得依赖占位页回退）；若发布二进制中嵌入的是占位页，流水线 MUST 失败且
MUST NOT 发布。

#### Scenario: 测试未通过阻断发布

- **WHEN** 任一平台的 `cargo test` 失败
- **THEN** 流水线失败，不产生任何 release

#### Scenario: 测试通过后进入构建

- **WHEN** ubuntu 与 windows 的测试均通过
- **THEN** 构建矩阵开始编译三个发布目标

#### Scenario: 真实控制台被嵌入

- **WHEN** 发布构建完成
- **THEN** 三个产物中嵌入的是真实 Web 控制台资源，可用 `ak daemon`
      启动并在浏览器加载控制台页面

#### Scenario: 占位页视为失败

- **WHEN** 构建环境中前端工具链不可用导致产物内嵌占位页
- **THEN** 流水线失败并说明原因，不发布该 tag 的 release
