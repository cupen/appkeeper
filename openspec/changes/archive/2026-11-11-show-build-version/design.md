## Context

- 品牌位当前是 `<a class="brand">` + `<span class="name">appkeeper<small>console</small></span>`；
  小字固定为 `console`，与版本无关。`ak-overview-bar` 里已有守护进程版本，但侧栏没有。
- 构建期没有任何 git/时间/目标平台信息：`build.rs` 只负责前端打包与 `AK_WEBUI_STAMP`，
  且 `BuildMode::Skip` 分支会提前 `return`。
- 守护进程概况 `DaemonInfo`（`src/server.rs`）在 `status_doc()` 与 `/v1/status` 处理器里
  各字面量构造一次；WS 快照走 `status_doc()`，所以两处必须同步改，否则三面漂移。
- 工程没有 `chrono`/`time` 依赖；为构建脚本引一个时间库不划算。
- 前端 `StatusDoc` 类型在 `webui/src/lib/types.ts`；`app-shell.test.ts` 的 `doc()` 夹具与
  若干视图测试夹具都构造 `DaemonInfo`，新增必填字段会要求它们同步。
- 现有 e2e 浏览器段已有 `brand mark reads ak` 钩子（`xtask/src/e2e.rs`），可扩展同一段。

## Goals / Non-Goals

**Goals:**

- 侧栏品牌位显示当前二进制版本号；双击（或键盘等价操作）看到这一次构建的完整身份。
- 构建元数据由二进制自带，经既有状态投影到达控制台，不新增端点、不改鉴权。
- git/时间/编译器信息不可得时优雅降级，绝不阻断构建或渲染。

**Non-Goals:**

- 不做 `ak version --verbose`、更新检查、复制到剪贴板或版本历史。
- 不把构建信息加进免鉴权的 `/v1/health`；它只随已鉴权的状态投影走。
- 不改变版本号语义（仍是 `CARGO_PKG_VERSION`）。

## Decisions

### D1 构建元数据在 `build.rs` 采集，先于打包分支

在 `main()` 开头、`match build_mode()` 之前调用 `emit_build_metadata()`，保证
`BuildMode::Skip` 也带上元数据。输出四个 `cargo:rustc-env`：

- `AK_BUILD_GIT`：`git rev-parse --short=12 HEAD`；再以 `git status --porcelain
  --untracked-files=no` 判断已跟踪文件是否有改动，有则追加 `-dirty`。
- `AK_BUILD_TIME`：构建时刻的 RFC 3339 UTC。优先 `SOURCE_DATE_EPOCH`（可复现构建），
  否则取系统时钟；用一段无依赖的 epoch→civil 换算，不引入 `chrono`/`time`。
- `AK_BUILD_TARGET`：Cargo 提供的 `TARGET`。
- `AK_BUILD_RUSTC`：`rustc --version` 输出；命令失败则 `unknown`。

`.git/HEAD` 与 `.git/refs`（存在时）加入 `cargo:rerun-if-changed`，否则仅切换提交不会
重新触发构建脚本。任何命令失败都回退 `unknown`，只打印一次 `cargo:warning`，不失败。

### D2 `BuildInfo` 类型与单一构造函数

`DaemonInfo` 增加 `build: BuildInfo`，`BuildInfo` 为四个 `String` 字段（serde snake_case）。
把两处重复的 `DaemonInfo` 字面量收敛为 `daemon_info(st, sup)`，`status_doc()` 与
`/v1/status` 共用它，从结构上消除三面漂移。`version` 仍保留在顶层，旧消费者无感。

备选：把 build 字段平铺进 DaemonInfo——旧字段与新字段混杂，序列化契约更难读，否决。

### D3 侧栏版本位

品牌结构改为 `.brand-block`（列方向）：

```
<a class="brand" href="/"> mark + name </a>
<button class="version">v0.1.0</button>
```

按钮左对齐到应用名（`margin-left: 28px + var(--ak-space-3)`），复用现有小号灰字
（`--ak-fs-xs` / `--ak-text-faint`），不改变品牌行高。点击不触发导航（按钮在锚点外）；
`title` 与 `aria-label` 说明「双击查看版本详情」，`aria-haspopup="dialog"`。双击打开，
Enter/Space 等价（阻止默认 click 行为，避免单次回车误触两次）。≤720px 与旧 `small` 一致隐藏。

版本取 `doc.daemon.version`；无投影时显示 `v—`，不伪造版本。

### D4 关于对话框

新增 `components/version-dialog.ts`（`ak-version-dialog`），输入 `daemon: DaemonInfo | null`：

- 原生 `<dialog>` + `showModal()`（不可用时退回 `open` 属性），关闭按钮与 Esc 均关闭；
  打开时焦点落在关闭按钮，关闭后回到版本位。
- 内容两段：构建（版本、git 提交、构建时间、目标平台、Rust 版本）与运行（控制端点、
  配置来源、运行时长、应用数/程序数）。构建时间按浏览器本地时区渲染；`unknown` 原样显示。
- 无投影时只显示「当前未连接守护进程」，不渲染空的构建/运行表。

备选：用 Web Awesome `wa-dialog`——多一个组件依赖且样式需覆盖，原生 dialog 足够，否决。

### D5 类型与测试同步

- `types.ts` 增加 `BuildInfo`，`DaemonInfo.build` 必填；所有测试夹具补一份 `build`。
- Rust：在既有投影测试中断言 `daemon.build` 四字段非空、`version` 与包版本一致。
- webui：`app-shell.test.ts` 断言版本位文本、双击打开、Esc 关闭；新增
  `version-dialog.test.ts` 覆盖字段渲染、unknown 与 null（离线）分支。
- e2e：在 `brand mark reads ak` 之后断言版本位匹配 `^v\d`，双击后对话框含「git 提交」
  与构建值，Esc 后消失。

## Risks / Trade-offs

- [本地构建总是 `-dirty`，看起来像发布版有改动] → 只统计已跟踪改动，未跟踪文件不计；
  该后缀本身就是构建事实，CI 干净构建不会出现。
- [build.rs 访问 git/rustc 拖慢构建] → 三者都是毫秒级；失败即回退，且不因失败重跑。
- [`.git/HEAD` 变化触发重编译] → 只在提交切换时发生，可接受；无 `.git` 的源码包不回退失败。
- [构建时间使二进制不可复现] → 支持 `SOURCE_DATE_EPOCH`，发布流水线可固定。
- [happy-dom 的 dialog 支持不完整] → 组件提供 `open` 属性回退路径，单测不依赖 `showModal`。

## Migration Plan

1. 先落 `build.rs` + `DaemonInfo`/类型，跑 Rust 测试确认投影字段。
2. 再做侧栏版本位与对话框，更新单测；最后补 e2e 双击断言。
3. 纯增量字段与 UI，无数据迁移；回滚为整次 revert。
4. 归档前 `openspec validate --specs` 确认 delta 无损。

## Assumptions

- 版本号以守护进程投影为准（前端与守护进程同一次构建，二者一致）。
- 构建时间用 UTC 存储、浏览器本地时区展示；`unknown` 不翻译。
- ≤720px 顶部条不显示版本位（与旧品牌小字一致）。
