## 1. 设计令牌与状态基础

- [x] 1.1 在 `webui/src/styles/tokens.css` 增加字号档位（`--ak-fs-xs/sm/md/lg/xl`）与控件高度 `--ak-control-h`，亮色重映射同步；验证：`pnpm exec tsc --noEmit` 通过，页面在暗/亮两主题下无样式丢失
- [x] 1.2 在 `webui/src/lib/format.ts` 实现 `stateLabel(slug)`（running→运行中、starting→启动中、stopping→停止中、backoff→退避中、exited→已退出、stopped→已停止、fatal→故障，未知原样）；验证：新增 `format.test.ts` 用例覆盖全部映射与未知 slug
- [x] 1.3 在 `webui/src/components/icons.ts` 增加 7 个 12px 状态图标并导出 `statusIcon(slug)`；验证：单元测试断言每个 slug 返回非空模板、未知 slug 返回 `nothing`
- [x] 1.4 重构 `status-badge.ts`：图标槽位固定宽度、支持 `label/glyph` 仍由调用方传入、未知 slug 不渲染空徽章；验证：`status-badge.test.ts` 断言文字、`data-status`、图标槽位宽度与未知 slug 回退

## 2. 外壳、内容列与侧栏

- [x] 2.1 调整 `app-shell.ts`：`main` 承载唯一水平内边距，视图与 `ak-overview-bar` 不再各自加水平 padding；验证：e2e 几何断言（顶栏与标题左边缘差 ≤1px）
- [x] 2.2 重写 `overview-bar.ts` 为显式网格：≥1120px 单行、720–1120px 两行、≤720px 单列；配置路径截断并带 `title`；刷新选择器右对齐；验证：1440/1024/820 三档截图无孤立控件、无横向滚动
- [x] 2.3 重写 `console-tree.ts`：程序名中性色、行首 6px 状态圆点、`title`/`aria-label` 含中文状态、名称单行省略、计数固定行尾；验证：树单测断言长名行高与短名一致、无状态色文字、`aria-label` 含状态词

## 3. 共享程序表与总览

- [x] 3.1 新增 `components/program-table.ts`（列：程序/应用/状态/重启/uptime/CPU/内存/stdout 速率/stderr 速率；数字右对齐；fatal 行醒目；可选行内快捷操作；预留固定宽度操作列）；验证：新增组件单测断言列顺序、占位符、`data-status` 与操作列不改变行宽
- [x] 3.2 为 `program-table` 接入行内 start/stop/restart：hover/focus-within 显现，破坏性操作 `window.confirm`，按钮点击阻止行导航，失败显示 `role=alert` 提示；验证：单测用桩 store 断言确认前无请求、成功后反馈、点击按钮不触发导航
- [x] 3.3 重写 `views/dashboard.ts` 为全局程序总表：按 app 分组、fatal/backoff 优先、分组标题含程序数、行点击进入详情、保留离线/轮询横幅；验证：更新 `dashboard.test.ts` 断言分组、状态词、排序与空态
- [x] 3.4 总览接入速率窗口切换（表标题栏，与 `prefs` 共享），移除与侧栏重复的 app 卡片列表；验证：单测断言切换后速率列取值随窗口变化，页面无旧卡片结构

## 4. App 概况

- [x] 4.1 `views/app-overview.ts` 改用 `program-table`，标题区保留应用级 apply 与「移除应用」，速率窗口移入表标题栏；验证：`app-overview-remove.test.ts` 与 apply 相关断言更新后通过
- [x] 4.2 状态列确保真实渲染中文徽章，pending 标记与 fatal 行样式与共享表一致；验证：单测断言状态列文本非空且含「运行中/故障」等词
- [x] 4.3 统一 page-head 结构与按钮高度（移除危险按钮与普通操作同高、同圆角）；验证：tsc + 单测通过，1440 截图目视无高度参差

## 5. 进程详情单列

- [x] 5.1 重构 `program-detail.ts` 为单列：标题行（面包屑/h1/徽章/操作）→ 6 项紧凑指标条 → mono 命令块（含工作目录）→ fatal/last_exit 提示；验证：`program-detail` 相关单测断言结构顺序与无重复 glyph
- [x] 5.2 让日志查看器 `flex:1; min-height:260px` 占据剩余高度，仅调整宿主/工具条样式，不改 `.scroller` 追加与滚动补偿逻辑；验证：900px 视口 e2e 断言日志宿主底部贴近内容区底部、日志仍实时追加
- [x] 5.3 详情页操作区与顶栏刷新/速率控件高度对齐，窄屏（≤720px）指标条折为两行；验证：820px 截图无横向滚动、按钮不重叠

## 6. 测试与 e2e 同步

- [x] 6.1 更新受影响的 vitest：`app-shell.test.ts`、`console.test.ts`、`dashboard.test.ts`、`app-overview-remove.test.ts`、`a11y.test.ts`，对新结构保留既有稳定钩子（`data-kind`/`data-status`/`.apply-strip`/`.pending`/`.brand .mark`）；验证：`pnpm test` 全绿
- [x] 6.2 更新 `xtask/src/e2e.rs` 中 webui playwright 脚本：总览/概况/详情选择器、pending 徽章位置、行内快捷操作与几何断言（左边缘 ≤1px、1024/820 无横向滚动、长名单行）；验证：`cargo run -p xtask -- e2e` 浏览器段通过
- [x] 6.3 增加布局回归断言（可在 e2e 内用 `getBoundingClientRect`）：1440 顶栏与内容左边缘、长应用名行高、状态列非空；验证：断言在改动前后分别红/绿（先失败后通过）

## 7. 构建与验收

- [x] 7.1 `cd webui && pnpm exec tsc --noEmit && pnpm test && pnpm build` 全绿，确认 `dist/` 更新
- [x] 7.2 `cargo build` 后以临时 workspace 启动 daemon（含 `[webui]`、running/fatal/backoff 三态程序），在 1440/1024/820 与暗/亮主题逐页核对：无空白状态列、无 20px 错边、无横向滚动、长名不换行
- [x] 7.3 `cargo run -p xtask -- e2e` 全量通过（含 remove 守卫、bench 冒烟与浏览器段）
- [x] 7.4 需要时 `scripts/docker-test.sh --stages e2e` 复跑容器内全量验收；记录结果（本机宿主全量 e2e 已通过，未额外触发容器复跑）
- [x] 7.5 实现完成后 `openspec validate --specs` 确认 delta 可无损归档（归档动作留给收尾流程）
