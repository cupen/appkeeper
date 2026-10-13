## Context

见 proposal.md 的 Why。此处只补充约束实现方案的现状事实：

- 发布物是 workspace 根包的单个 bin `ak`（crate `appkeeper`，`default-run = "ak"`）；
  workspace 另有 `xtask`（验收 e2e）与 `appkeeper-bench`（基准测量），二者不随发布分发。
- release profile 为 `opt-level=3 / lto="fat" / codegen-units=1 / strip="symbols"`，
  单目标编译在 runner 上是分钟级，且三个目标必须并行。
- `build.rs` 在 release 模式下会自动 `pnpm install --frozen-lockfile` + `pnpm build`
  并把 `webui/dist` 内嵌；Node 工具链缺失或构建失败时**静默**写入占位页并只发
  `cargo:warning`，构建本身仍成功——占位页混入发布产物不会自动让构建失败，
  必须由流水线显式检出。`webui/package.json` 用 `packageManager: pnpm@11.24.0`
  固定了 pnpm 版本。
- `Cargo.lock` 中没有任何 C 依赖（无 `openssl-sys` / `libz-sys` / `ring`），musl
  静态链接不需要额外 cc 工具链。
- `clap` 的 `version` 属性取自 `Cargo.toml`，`ak --version` 输出即 crate 版本。
- 现有 `ci.yml` 只在 `push: main` 与 PR 上跑 `cargo test`，且不覆盖 tag push。

## Goals / Non-Goals

**Goals:**

- 一条命令（`git push origin v<version>`）即可产出并公开三平台发布物，全程无需
  人工撰写说明或点确认。
- 发布出去的二进制一定能通过"下载 → 校验 → 运行"的自检，且自检步骤可被 spec
  场景逐条验证。
- 发布物与源码版本严格对齐，tag 与 `Cargo.toml` 漂移时宁可失败也不发布。
- 流水线对既有 spec 无侵入：`ci.yml`、`webui-build` 的构建契约都不改。

**Non-Goals:**

- 不做 nightly / 滚动发布、不做多版本并存。
- 不引入 `rust-toolchain.toml`、不固定工具链版本（沿用 `dtolnay/rust-toolchain@stable`）。
- 不做包管理器分发（homebrew / scoop / winget）。
- 不改应用自身的版本管理流程（版本号仍手工在 `Cargo.toml` 里维护后再打 tag）。
- 不把 `xtask` / `appkeeper-bench` / `conf/` 示例配置打进发布包。

## Decisions

### D1: 触发用 tag push，不用 workflow_dispatch 或 Release 事件

tag push 是版本与产物一一对应的天然锚点，`v*` 的 filter 同时天然排除了非版本 tag。
备选的 `workflow_dispatch` 需要人手填版本号并失去"tag 即意图"的审计链；`release:
types: [published]` 要求先在网页上建空 release，CI 反而成了附件上传器，版本校验
也得在事后做。选 tag push。

### D2: tag 为版本真源，做只读校验而非自动改写

verify 阶段在干净 checkout 里比对 `Cargo.toml` 的 `version` 与 tag 值（去掉前导 `v`），
不一致直接失败。备选"以 Cargo.toml 为准"会让二进制版本与发布标签悄悄错位；备选
"按 tag 自动改写 Cargo.toml"能在实现里做到，但它把版本号变成了 CI 的输出，
本地 `ak --version` 与仓库内容在打 tag 到构建之间会有一段不一致窗口，且会在
build 阶段引入对源码的写操作。只读校验是唯一无副作用的选择。

### D3: 用 target 三元组命名归档

两个 Linux 产物（gnu / musl）若都用 `linux-x86_64` 会撞名。直接用
`ak-<version>-<target-triple>.<ext>`，与 cargo 的 target 概念一一对应，
用户按 `rustup target list` 就能对上，也省掉 CI 里的 platform→文件名映射表。
扩展名按平台惯例分：Linux `.tar.gz`，Windows `.zip`（Windows 上 `tar` 不可靠）。

### D4: 单独一个 verify job 作为门禁，再进构建矩阵

`verify` job（ubuntu + windows 矩阵）先跑 `cargo test`，两个平台都绿了才放行
`build` 矩阵（`needs: verify`）。备选是让每个 build job 各自跑一遍测试——重复
消耗约 2 倍 CI 时间；备选"依赖 ci.yml"不可行，跨 workflow 复用必须要
`workflow_call` 重构现有 ci.yml，代价大于收益。发布 workflow 自带门禁的代价是
tag push 会再跑一遍测试（main 上已经跑过），换来的是 tag 指向任意历史提交时
仍有门禁兜底，这个代价可以接受。

### D5: webui 占位页必须被显式检出，不能只看构建是否成功

`build.rs` 的回退只发 `cargo:warning`，构建照样成功，所以"构建没报错"不等于
"控制台在里面"。构建 job 在装好 Node + pnpm 后执行真实前端构建，并在打包前
显式断言 `webui/dist/index.html` 存在且 `webui/dist/.ak-placeholder` 标记不存在，
两者任一命中就让该 job 失败。这让"占位页视为失败"成为可测试的脚本契约，而不是
一句"注意别忘了装 Node"的祈祷。

### D6: 矩阵编译 + 汇总 job 用 gh CLI 一次性发布

三个 build job 各自把归档作为 artifact 上传，汇总 job `download-artifact` 拉齐后：
计算 SHA256 → 生成 `SHA256SUMS` → 拼资产清单与校验和段落 → `gh release create`
一次带全部 `assets#`。备选 `softprops/action-gh-release` 少写点 YAML，但它在矩阵
场景下要靠 `files:` 表达式吃下多 job 产物，且自定义正文拼接仍需额外步骤，净收益
不大；备选"各 job 各自创建/追加 release"存在并发写同一 release 的竞态。
`gh` 是 runner 预装的，`GH_TOKEN: ${{ github.token }}` 免去额外 secret 配置。

### D7: 校验和用单个 SHA256SUMS，格式交给标准工具

一个 `SHA256SUMS` 覆盖全部资产，`sha256sum -c` 可一次校验全量；Windows 侧用
`sha256sum -c` 需要 coreutils，但格式本身是跨平台事实标准，且正文里内嵌同一份
内容让"不想装工具的用户"也能肉眼比对。备选"每资产一个 .sha256 sidecar"会产生
资产数量翻倍，且用户需要逐个下载校验文件。

### D8: prerelease 由 tag 形态判定

`v*` tag 的版本部分含 `-rc` / `-beta` / `-alpha` 即 `--prerelease`，否则正式发布，
不走 draft。draft 会给"流水线已成功但东西还没公开"留出人机不一致的中间态，与
"推 tag 即发布"的意图相悖。

### D9: 构建目标矩阵在 runner 层面展开而非交叉编译

musl 与 gnu 都在 `ubuntu-24.04` 上原生构建，msvc 在 `windows-latest` 上原生构建。
三个目标恰好落在两个 OS 上，矩阵只需按 OS 分组，Windows 只跑一次（省掉一次
双倍编译）。交叉编译 Windows 或在 Linux 上装 mingw 都要额外工具链且无法复用
windows runner 上已验证的测试结果。

## Risks / Trade-offs

- **LTO 编译慢导致 workflow 超时** → 三个目标并行（最长路径 = 单次 fat-LTO 编译，
  约 10 分钟量级），并在 job 级不设额外超时；必要时可缓存 cargo 产物。
- **`actions/upload-artifact` v4 默认排除隐藏文件** → 打包目录里不放点文件，
  归档只含 `ak`/`ak.exe`、`LICENSE`、`README.md`；`dist/` 是构建中间物，不入包。
- **webui 依赖安装失败（pnpm 网络/registry）导致发布失败** → 这是期望行为（宁可不发），
  但要在失败信息里指出是前端依赖安装环节，而非让人误以为是 Rust 编译问题。
- **首次发布时 `--generate-notes` 无对比基线** → 自动变更列表可能为空，属正常，
  资产清单与校验和段落仍会生成（spec 已覆盖该场景）。
- **tag 与 Cargo.toml 漂移导致发布被卡住** → 失败信息直接打印两边的版本值与修复
  动作（先改 `Cargo.toml` 再重打 tag），并在 README 记录发布流程顺序。
- **发出去的 release 无法撤回** → 因此设前置门禁（测试 + 真实 webui）而非事后补救；
  若确需撤回，`gh release delete` 是人工操作，README 中不做自动化承诺。

## Migration Plan

1. 合并 `release.yml` 与 README 更新到 main。
2. 在 `Cargo.toml` 确认 `version` 为期望发布的版本值，提交。
3. `git tag v<version> && git push origin v<version>`。
4. 流水线先跑 verify（双平台测试），再并行构建三目标，最后创建公开 release。
5. 回滚策略：若需撤回，删除该 tag 与对应 release（`gh release delete`），修正
   `Cargo.toml` 版本后重新打 tag；workflow 本身无需回滚，删除文件即可恢复原状。

## Open Questions

无。所有会改变 spec 或任务拆解的问题已在拷问阶段定稿；剩余细节（CI 缓存策略的
具体 key、README 章节的措辞）属于实现自由度内。