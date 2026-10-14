## Why

appkeeper 目前只能在源码机器上 `cargo build --release` 得到二进制，仓库里没有任何
可分发的构建产物：没有 git tag、没有发布流水线、没有跨平台产物。目标用户拿不到
一个开箱即用的 `ak`，也无法验证下载物的完整性——在 alpine/各类 Linux 发行版上
还要自己配交叉编译环境。本次变更建立"推 tag 即发布"的 GitHub Release 流水线，
一次产出 Linux（静态 musl + glibc）与 Windows x86_64 三份归档包，并给出可核验的
SHA256 校验和。

## What Changes

- 新增 `.github/workflows/release.yml`：推送 `v*` tag 触发发布流水线
- 版本一致性门禁：tag 版本与 `Cargo.toml` 的 `version` 不一致即构建失败，
  保证 `ak --version` 输出与发布 tag 对得上
- 三目标构建矩阵：
  - `x86_64-unknown-linux-musl`（静态链接，单文件，不依赖 glibc）
  - `x86_64-unknown-linux-gnu`（ubuntu-24.04 runner）
  - `x86_64-pc-windows-msvc`（windows-latest）
- 归档约定：`ak-<version>-<platform>-<arch>.<tar.gz|zip>`，包内含可执行文件
  （`ak` / `ak.exe`）+ `LICENSE` + `README.md`
- 校验和：汇总为一个标准格式的 `SHA256SUMS` 作为 release asset 上传，同一内容
  以代码块形式嵌入 release notes 正文
- 发布说明：`--generate-notes` 生成 commit 列表，脚本追加资产清单与校验和段落
- 质量门禁：发布 job 前置跑 `cargo test`（ubuntu + windows），且 release 构建必须
  真实构建 webui（Node ≥ 22 + pnpm 11.24.0），禁止占位页回退进入发布产物
- 发布形态：tag 含 `-rc`/`-beta`/`-alpha` 自动标记 prerelease，否则直接公开发布
  （不走 draft），权限收敛为 `contents: write` + `GITHUB_TOKEN`
- README 新增"安装与校验"小节：从 Releases 按平台下载、`sha256sum -c SHA256SUMS`
  校验、再运行 `ak --version`

不涉及：nightly/滚动发布、arm64 Linux、i686 Windows、包管理器分发
（homebrew/scoop）、版本号维护流程本身的改造。

## Capabilities

### New Capabilities
- `release-pipeline`: GitHub Release 发布流水线的可观察契约——触发条件、版本一致性
  门禁、构建目标矩阵与归档命名、校验和与发布说明内容、发布形态（正式/预发布）、
  发布前质量门禁（测试通过 + 真实 webui 嵌入）

### Modified Capabilities
- 无。既有能力的 requirement 不变：`webui-build` 的自动构建与占位页回退契约原样
  保留（回退仍允许，只是不允许出现在发布产物里——这是发布流水线的约束，
  属于新能力）；`ci.yml` 的测试矩阵不动。

## Impact

- 新增文件：`.github/workflows/release.yml`（本变更的主要产物）
- 修改文件：`README.md`（新增安装与校验小节）
- 不触碰 Rust 源码与 `Cargo.toml` 的依赖/配置；`version` 字段是版本真源的对照面
- 外部依赖：GitHub Actions（checkout / setup-node / pnpm setup / upload-artifact /
  download-artifact）、runner 预装 `gh` CLI
- 对构建时长敏感：release profile 为 `lto = "fat"` + `codegen-units = 1`，加上
  webui 真实构建，单个 job 预计 10 分钟量级，靠矩阵并行控制总时长