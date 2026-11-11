## Why

侧栏品牌位现在固定显示「CONSOLE」，控制台里没有任何地方能看到这一份二进制究竟是哪个提交、
何时、为何种目标平台构建的。排查线上问题时只能靠 `ak --version` 拿到一个语义化版本号，
同一版本的不同构建无法区分。把版本号放到品牌位，并让双击展示构建元数据，能让「我看到的
控制台」与「我报告的问题」精确对应到一次构建。

## What Changes

- 构建脚本在编译期采集并注入：git 提交（含 dirty 标记）、构建时间（UTC）、目标三元组、
  Rust 编译器版本；任一不可得时回退为 `unknown`，不阻断构建。
- 守护进程状态投影的 `daemon` 对象新增只读 `build` 字段；`/v1/status`、`/api/overview`
  与 `/ws` 快照同源携带，旧客户端忽略新字段即可（向后兼容，无 **BREAKING**）。
- 侧栏品牌位的小字从 `CONSOLE` 改为 `v<版本号>`（沿用现有小号灰字样式，≤720px 仍隐藏）。
- 双击版本号弹出「关于 appkeeper」原生对话框，展示版本、git 提交、构建时间、目标平台、
  Rust 版本，以及当前守护进程的控制端点、配置来源、运行时长、应用/程序数；键盘
  Enter/Space 等价触发，Esc 或关闭按钮退出。
- 守护进程离线且从未拿到投影时，版本位显示占位符，双击对话框说明「未连接」，不报错。

## Capabilities

### New Capabilities

<!-- 无新增能力：改动落在既有控制台 UI 与 API 契约上。 -->

### Modified Capabilities

- `webui-ui`: 左侧导航的品牌位新增版本号展示与「关于」对话框的触发方式和内容要求。
- `webui-api`: 守护进程状态投影新增构建元数据字段，并要求 `/v1`、`/api`、WS 同源同构。

## Impact

- 构建：`build.rs` 新增构建元数据采集与 `cargo:rustc-env` 注入；不改变既有 webui 打包流程。
- 后端：`src/server.rs` 的 `DaemonInfo` 增加嵌套 `build` 字段（同源投影自动覆盖 `/v1`、`/api`、
  WS），补充投影单测。
- 前端：`webui/src/lib/types.ts`、`webui/src/app-shell.ts`，新增
  `webui/src/components/version-dialog.ts`；不改变路由与现有视图结构。
- 测试：新增/更新 webui 单测（版本位、双击与键盘触发、字段渲染、离线回退）与 xtask e2e
  浏览器段的双击断言；`cargo test --workspace` 覆盖投影字段。
- 兼容：状态 JSON 只增字段；CLI、配置、持久化格式与既有 API 语义不变。
