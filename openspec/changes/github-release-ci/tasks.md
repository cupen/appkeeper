## 1. Workflow 骨架与触发

- [x] 1.1 新建 `.github/workflows/release.yml`，声明 `on: push: tags: ['v*']`
      （其他触发一律不注册），顶层 `permissions: contents: write`，验证：
      `actionlint .github/workflows/release.yml`（或 GitHub 侧语法检查）无告警，
      且 `push` 非 `v*` tag 不会触发本 workflow
- [x] 1.2 在 workflow 内新增 `verify` job：checkout（`fetch-depth: 0`）→ 读取
      `${GITHUB_REF_NAME}` 去掉前导 `v` 得到 tag 版本 → 与 `Cargo.toml` 的
      `version` 比对，不一致时打印两侧值与修复提示并 `exit 1`；验证：
      构造 `v9.9.9` 的本地 tag 推送到测试分支的 workflow_dispatch 模拟运行，
      或在 PR 上对该 job 的比对脚本做 shell 级单测，确认漂移时退出码非 0
- [x] 1.3 在 `verify` job 内加 `cargo test` 步骤，矩阵为
      `[ubuntu-latest, windows-latest]`，复用 `dtolnay/rust-toolchain@stable` 与
      `Swatinem/rust-cache@v2`；验证：任一平台测试失败时该 job 红

## 2. 三目标构建与归档

- [x] 2.1 新增 `build` job（`needs: verify`），矩阵三条：
      `x86_64-unknown-linux-musl` 与 `x86_64-unknown-linux-gnu` 跑在
      `ubuntu-24.04`，`x86_64-pc-windows-msvc` 跑在 `windows-latest`；用
      `runs-on: ${{ matrix.os }}` + `target: ${{ matrix.target }}` 表达，
      验证：workflow 展开后 Windows 目标只被调度一次
- [x] 2.2 在 build job 内完成工具链准备：`rustup target add ${{ matrix.target }}`；
      Linux 侧额外装 `actions/setup-node`（node-version 22）+ `pnpm/action-setup`
      （version 取 `webui/package.json` 的 `packageManager`，即 11.24.0），
      保证 `build.rs` 在 release 模式下能真实跑前端构建；验证：
      job 日志中可见 `pnpm install --frozen-lockfile` 与 `pnpm build` 的执行
- [x] 2.3 在打包前加占位页断言：`test -f webui/dist/index.html` 且
      `test ! -f webui/dist/.ak-placeholder`（Windows 侧用等价断言），
      任一失败即让 job 失败并输出"前端未真实构建"提示；验证：
      人为设置 `AK_WEBUI_BUILD=skip` 跑一次该断言，确认断言失败
- [x] 2.4 编译产物：Linux 用 `cargo build --release --target ${{ matrix.target }} -p appkeeper --bin ak`，
      Windows 侧同样参数；验证：产物路径
      `target/<triple>/release/ak[.exe]` 存在且 `ak --version` 在 Linux 目标上
      可执行、输出与 tag 版本一致
- [x] 2.5 打包：组装暂存目录（`ak` 或 `ak.exe` + 仓库根的 `LICENSE` +
      `README.md`），Linux 用 `tar -czf`，Windows 用
      `Compress-Archive` 或 `tar -a -c -f ...zip`；文件名按 design D3：
      `ak-<version>-<target-triple>.tar.gz` / `.zip`；验证：解包后目录内
      恰含三个条目且 `ak --version` 能在解包目录中直接运行
- [x] 2.6 用 `actions/upload-artifact@v4` 上传每个归档（`if-no-files-found: error`），
      artifact 名与 target 三元组一致；验证：任一目标缺包时 job 失败

## 3. 汇总与发布

- [x] 3.1 新增 `release` job（`needs: build`）：下载全部 artifact 到同一目录，
      确认三个归档齐全，缺任一份即失败；验证：手工删掉一份产物重跑，确认失败
- [x] 3.2 在 release job 内计算 SHA256 并生成 `SHA256SUMS`：内容为
      `<hash>  <归档名>` 每行一条，仅含归档名不含路径；验证：
      本地对该目录执行 `sha256sum -c SHA256SUMS` 全部输出 `OK` 且退出码 0
- [x] 3.3 生成发布说明正文：先用 `gh api` 或 `--generate-notes` 拿到自动变更列表，
      再追加「资产」清单（每个归档的平台/架构/文件名）与「校验和」代码块
      （内嵌 `SHA256SUMS` 原文）；验证：正文代码块内容与 `SHA256SUMS` 文件逐字节一致
- [x] 3.4 调用 `gh release create "${{ github.ref_name }}" --title ... --notes-file <正文>`
      `--notes-file` 或 `--generate-notes` 二选一并追加自定义段落，
      `--verify-tag` 防 tag 被删，`GH_TOKEN: ${{ github.token }}`；
      版本含 `-rc` / `-beta` / `-alpha` 时附加 `--prerelease`；
      资产列表含三个归档 + `SHA256SUMS`；验证：手工触发一次本地等价命令，
      确认 release 非 draft、资产齐全
- [x] 3.5 确认发布失败路径：不设置 `continue-on-error`，任一前置 job 失败时
      `gh release create` 不会执行；验证：故意让一个 build 目标失败，
      确认 GitHub 上无该 tag 的 release

## 4. 文档

- [x] 4.1 在 `README.md` 新增「安装与校验」小节：从 Releases 按平台下载对应
      归档的示例命令（Linux `curl -LO` + `tar -xzf`、Windows 下载 zip）、
      `sha256sum -c SHA256SUMS` 校验步骤、`ak --version` 自检，
      并说明 musl 包适用于 alpine 等无 glibc 环境；验证：按文档命令在
      干净环境实测能跑通
- [x] 4.2 在 README 或 CONTRIBUTING 段落简述发布流程顺序：先改 `Cargo.toml`
      的 `version` → 提交 → 打 `v<version>` tag → 推送 tag 触发发布，
      并注明 tag 与版本不一致会被流水线拒绝；验证：文档与 design D2 的
      修复提示措辞一致