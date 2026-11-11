## 1. 构建元数据

- [x] 1.1 在 `build.rs` 的 `main()` 开头（`build_mode()` 分派之前）新增 `emit_build_metadata()`：采集 `AK_BUILD_GIT`（短哈希 + 已跟踪改动时 `-dirty`）、`AK_BUILD_TIME`（`SOURCE_DATE_EPOCH` 优先，否则系统时钟，epoch→RFC 3339 UTC，无新依赖）、`AK_BUILD_TARGET`（`TARGET`）、`AK_BUILD_RUSTC`（`rustc --version`）；任一失败回退 `unknown` 并使构建继续；存在 `.git` 时把 `.git/HEAD`、`.git/refs` 加入 `cargo:rerun-if-changed`。验证：`cargo build` 后通过 `strings target/debug/appkeeper | grep AK_BUILD` 或临时打印确认四个变量非空；在无 `.git` 的副本中构建不失败
- [x] 1.2 在 `src/server.rs` 新增 `BuildInfo`（`git_commit`/`built_at`/`target`/`rustc`，serde snake_case），`DaemonInfo` 增加 `build: BuildInfo`；抽取 `daemon_info(st, sup)` 并让 `status_doc()` 与 `/v1/status` 共用，消除两处字面量重复。验证：`cargo check --all-targets` 通过，投影相关单测通过
- [x] 1.3 在既有状态投影测试中断言 `daemon.build` 四字段非空、`daemon.version == env!("CARGO_PKG_VERSION")`，并断言 `/v1/status` 与 `status_doc()` 的 `build` 完全一致。验证：`cargo test --workspace` 通过

## 2. 前端类型与夹具

- [x] 2.1 在 `webui/src/lib/types.ts` 增加 `BuildInfo` 接口并给 `DaemonInfo` 加必填 `build`；同步更新 `app-shell.test.ts`、`apply-ui.test.ts`、`console.test.ts`、`dashboard.test.ts`、`app-overview-remove.test.ts` 等夹具。验证：`pnpm exec tsc --noEmit` 与 `pnpm test` 通过

## 3. 侧栏版本位与关于对话框

- [x] 3.1 调整 `webui/src/app-shell.ts` 品牌结构为 `.brand-block`（锚点 + 独立版本按钮），版本按钮显示 `v<version>`（无投影时 `v—`），左对齐应用名、复用 `--ak-fs-xs`/`--ak-text-faint`，≤720px 隐藏；按钮带 `aria-haspopup="dialog"`、`title`/`aria-label`「双击查看版本详情」，单击不导航。验证：`app-shell.test.ts` 断言版本文本与品牌 mark 仍为 `ak`，1440/820 几何无新增换行
- [x] 3.2 新增 `webui/src/components/version-dialog.ts`：原生 `<dialog>`（不可用时回退 `open` 属性），展示构建段（版本/git 提交/构建时间/目标平台/Rust 版本）与运行段（控制端点/配置来源/运行时长/应用与程序数）；`unknown` 原样、构建时间按本地时区、`daemon=null` 时只显示「当前未连接守护进程」；关闭按钮与 Esc 关闭并把焦点还给版本位。验证：新增 `version-dialog.test.ts` 覆盖完整、unknown、离线三种输入与开关
- [x] 3.3 在 `app-shell.ts` 接入对话框：双击版本位打开，Enter/Space 等价（阻止默认 click 带来的重复触发），Esc/关闭后收起。验证：`app-shell.test.ts` 断言双击前无对话框、双击后出现、Esc 后消失且焦点回到版本按钮

## 4. 测试与 e2e

- [x] 4.1 更新受影响的 webui 单测并新增版本相关用例，确认既有品牌/侧栏断言不回归。验证：`pnpm test` 全绿（新增用例先红后绿）
- [x] 4.2 在 `xtask/src/e2e.rs` 浏览器段 `brand mark reads ak` 之后增加：版本位匹配 `^v\d`、双击后对话框含「git 提交」与构建值、按 Esc 后对话框消失。验证：`cargo run -p xtask -- e2e --no-browser` 之外的完整 e2e 浏览器段通过

## 5. 构建与验收

- [x] 5.1 `cd webui && pnpm exec tsc --noEmit && pnpm test && pnpm build` 后 `cargo build`，确认二进制内嵌新前端与构建元数据。验证：命令全绿，`dist/` 更新
- [x] 5.2 `cargo test --workspace` 与 `cargo run -p xtask -- e2e` 全量通过（含浏览器段）。验证：两条命令退出 0
- [x] 5.3 以临时 workspace 启动 daemon，在 1440/820 与暗/亮主题下核对：版本位不改品牌行高、双击弹窗内容正确、Esc 收起、窄屏隐藏。验证：几何测量与截图目视
- [x] 5.4 `openspec validate --specs` 确认 delta 可无损归档。验证：20 项全过，退出 0
