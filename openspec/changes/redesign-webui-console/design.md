## Context

见 `proposal.md` 的 Why。当前约束：

- 前端是 Lit + Web Awesome + Vite，样式分散在 `tokens.css`、`app.css`、`shared.ts` 与各组件
  shadow root 的 `css` 片段里；没有统一的字号/控件高度档位。
- 三个视图各自给 `:host` 加 `padding: 16px 20px`，外壳 `main` 又有 `padding: 12px 16px`，
  顶栏 `ak-overview-bar` 只吃到外壳内边距——这是 20px 错边的直接来源。
- `status-badge` 只在测试里被 import，生产构建中 `<ak-status-badge>` 是未注册标签，0×0。
- 日志查看器的 `.scroller` 由命令式追加 DOM（rAF 批处理），toolbar 与空态提示是 Lit 模板。
- `webui-ui` 已规定两棵树、三处状态语义、日志跟随与高速降级、pending/apply 与刷新频率；
  本次不改变这些行为，只改信息架构与呈现。
- 测试耦合：vitest 直接查 `.app-link`/`li[data-state]`/`.apply-strip` 等；`xtask` e2e 用
  `page.evaluate` 穿透 shadow root 查 `.bar`、`.pending .badge`、`.apply-strip button`、
  `.brand .mark`。重构必须同步这些选择器。

## Goals / Non-Goals

**Goals:**

- 用一套 token 与一条内容列，消除 20px 错边、行高抖动、长名换行与窄屏溢出。
- 总览改成全局程序总表；详情改成单列 + 日志占满；操作与选择器各归其位。
- 状态在三处真实渲染、词表统一为中文、颜色只用于指示器。
- 保持既有 API/WS/后端 projection 与 CLI 完全不变。

**Non-Goals:**

- 不引入图表库、sparkline 或历史趋势（投影只有瞬时值）。
- 不做应用搜索/筛选、批量选择、拖拽排序（可作为后续变更）。
- 不改 `webui-api`、`webui-build` 契约，不改后端字段。
- 不追求移动端优先；≤720px 只保证「侧栏收顶、内容单列、不横向溢出」。

## Decisions

### D1 状态徽章的注册与渲染归属

`status-badge.ts` 改为被三个消费视图显式 `import '../components/status-badge.js'`
（dashboard / app-overview / program-detail），而不是靠外壳隐式注册。理由：显式依赖让单测
拿到注册、避免「外壳重排导致组件失联」再次发生；模块只求值一次，重复 import 无成本。
备选：只在 `app-shell.ts` import——更省一行，但视图单测仍需自行 import，耦合反而隐蔽。

同时删除 dashboard 行内与详情标题旁的重复 Unicode glyph：形状由徽章统一提供，标题只保留
文字；App 概况表格的 name-cell 也不再单独放 glyph。

### D2 状态形状改用内联 SVG，不再用 Unicode 符号

在现有 `components/icons.ts` 增加 `status-running/starting/stopping/backoff/exited/stopped/fatal`
一组 12px stroke 图标（play / loader / square / rotate / square / stop / x），
`ak-status-badge` 以 `flex: 0 0 12px` 固定图标槽位，字形基线不再影响行高。徽章结构保持
「指示点 + 图标 + 状态词」，`data-status` 仍为英文 slug 供 CSS/测试钩子。
备选：保留 Unicode——零成本但宽度/基线依字体变化，正是当前「歪」的来源之一；纯圆点——
无法在灰度下区分状态。

侧栏只用 6px 着色圆点 + 中性文字，`title`/`aria-label` 给出「程序名 — 中文状态」。
这是在用户选定方向（中性文字 + 状态圆点）下对 `webui-visual-system` 灰度和可访问要求的
落实方式。

### D3 状态词中文化放在 `lib/format.ts`

新增 `stateLabel(slug)`：`running/starting/stopping/backoff/exited/stopped/fatal` 映射为
运行中/启动中/停止中/退避中/已退出/已停止/故障；未知 slug 原样返回。徽章 `label` 由调用方
传入，徽章自身仍不内置词表（保持后端/前端词表单一职责边界）。退避剩余秒数继续跟在徽章后
（`wait_reason`），不塞进徽章。

### D4 内容列：内边距只在外壳给一次

统一约定：`ak-app` 的 `main` 提供唯一水平内边距（建议 `12px 20px`）；`ak-overview-bar`
与三个视图的 `:host` 一律 `padding-left/right: 0`，视图只保留纵向间距。总览/概况/详情共用
同一内容列宽度。这样顶栏天然与内容左边缘对齐，不再依赖各视图复刻 padding。

备选：给每个视图加补偿 margin——数值会继续漂移，否决。

### D5 顶栏用显式网格而非 flex wrap

`.bar` 改为 CSS grid，分两档：

- ≥1120px：单行 7 列（版本、配置、运行、巡检、CPU、内存、刷新）。
- 720–1120px：两行网格（第一行版本/配置/运行/巡检，第二行 CPU/内存/刷新），
  行内元素按固定列宽对齐，不允许 `margin-left:auto` 造成孤立控件。
- ≤720px：并入侧栏收顶后的单列布局，仍按两行网格。

配置来源路径允许截断（`text-overflow: ellipsis`，`title` 给全路径），避免长路径撑破单行。
刷新频率选择留在顶栏；速率窗口移入程序表标题栏（App 概况与总览各一份，状态由 `prefs` 共享）。

### D6 共享程序表组件

新增 `components/program-table.ts`（presentational）：输入程序行、是否显示 app 列、速率窗口、
是否启用行内快捷操作；输出行点击导航。总览与 App 概况都用它，保证列宽、数字右对齐、
fatal 行样式、空态完全一致。理由：当前两处表格各写一套，是列宽/字号漂移的根源。
备选：复制两份 CSS——短期省事，长期继续漂移，否决。

表格列（顺序）：程序、应用（仅总览）、状态、重启、uptime、CPU、内存、stdout/s、stderr/s。
数字列 `text-align:right; font-variant-numeric: tabular-nums`；状态列固定最小宽度并
`white-space:nowrap`，避免中文状态词切换导致列宽跳动。

### D7 行内快捷操作

共享表格预留固定宽度操作列（约 96px，`opacity:0`，`tr:hover`/`tr:focus-within` 时显现），
按钮常驻布局但视觉隐藏，避免悬停时列宽变化。破坏性操作复用 `window.confirm`（与 apply/
移除应用一致），详情页保留现有内联确认。行点击导航与按钮点击用 `e.stopPropagation()` 隔离。
操作失败在表格上方以一条 `role=alert` 的提示条显示，复用现有 notice 样式。

### D8 详情页单列

结构：`header`（面包屑 + h1 + 徽章 + 操作）→ 指标条（6 个内联 kv，窄屏两行）
→ 命令块（mono，可横向滚动，不折行破坏对齐）→ fatal/last_exit 提示 → 日志查看器
`flex:1; min-height:260px`。视图 `:host` 保持 `overflow-y:auto`；日志查看器获得剩余高度。
备选：保留卡片网格——信息块高度不均、短内容也占整行，否决。

### D9 Token 收敛

在 `tokens.css` 增加字号档位（`--ak-fs-xs/sm/md/lg/xl`，对应 0.72/0.8/0.875/1/1.25rem）
与控件高度 `--ak-control-h: 28px`；所有组件字号/控件高度改取档位，间距只取现有
`--ak-space-*`（4/8/12/16/20/24/32/40），删除 6/9/10/14px 这类近值。颜色沿用现有语义 token，
状态色仅用于指示器；亮色重映射同步到新档位，保证两主题同构。

### D10 测试与 e2e 选择器策略

保留既有稳定钩子：`data-kind`、`data-status`、`data-fatal`、`.apply-strip`、`.pending`、
`.brand .mark`、树内 `a[href]`。新增 `data-app`/`data-program` 便于 e2e 精确定位行。
vitest 更新：dashboard 测试改为断言总表分组与状态词；app-overview 断言状态列非空；
program-detail 断言单列结构与日志宿主高度；新增 `stateLabel` 与 `program-table` 测试。
`xtask/src/e2e.rs` 的 webui 脚本同步改选择器与「待应用变更」文案位置，跑完整 e2e 验收。

## Risks / Trade-offs

- [重构 DOM 导致既有 vitest / xtask e2e 选择器失效] → D10 先列选择器清单，改一处更新一处；
  最终以 `pnpm test` + `cargo run -p xtask -- e2e` 全绿为准。
- [总览全局表在程序多时变长、滚动性能下降] → 沿用 Lit 常规渲染（无虚拟列表）；程序量在本
  工具的目标场景（几十个）内可接受；后续量大再加虚拟化。
- [中文状态词比英文宽，状态列抖动] → 状态列固定最小宽度 + 徽章 `nowrap`（D6）。
- [日志查看器命令式追加被布局改动干扰] → 只改 `:host`/`.toolbar` 样式与容器高度，
  不触碰 `.scroller` 追加、`scrollTop` 补偿与 rAF 批处理逻辑。
- [单行顶栏在长配置路径/中文字体下溢出] → 配置路径截断 + 显式网格断点；在 1440/1024/820
  三个宽度做几何回归（新增轻量布局测试或 e2e 断言）。
- [只改前端未重建 dist，二进制仍嵌旧 UI] → 验收步骤固定为
  `pnpm exec tsc --noEmit && pnpm test && pnpm build` 后再 `cargo build`。

## Migration Plan

1. 先落 token 与共享表组件，再逐个视图切换（dashboard → app-overview → program-detail），
   每步保持页面可运行。
2. 同步更新单测；最后更新 xtask e2e 脚本并跑全量验收（含浏览器段）。
3. 无数据/接口迁移；回滚为整次 revert（纯前端，无持久化影响）。
4. 归档前跑 `openspec validate --specs` 确认 delta 无损合入。

## Assumptions

- 亮色主题保留并与暗色同步打磨（用户未否决；默认仍为暗色）。
- 未答的第 2 轮问题按推荐项执行：总览=全局程序总表、详情=单列 + 日志占满、状态词=中文、
  行内操作=hover 显现。
- 保留 `/app/:app` 与 `/app/:app/program/:program` 路由与深链行为，保证 e2e 与书签不失效。
