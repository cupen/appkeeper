# Design — refine-webui-log-and-actions

## Context

webui 为 Lit 组件 SPA，现状与本设计相关的骨架：

- 操作入口分散两处：`program-table.ts` 行内 `.op` 按钮组（悬停浮现、`window.confirm`
  确认、`actionsFor(state)` 状态机）与 `program-detail.ts` 头部按钮组（inline confirm
  span）。两处各自维护 busy/feedback。
- `log-viewer.ts` 配色钉死 `var(--ak-well)`（跟随应用主题）；空态渲染「等待输出…」。
- `overview-bar.ts` 概况条为 3 档断点（1120/720px）grid，含「版本」格；侧栏品牌位
  （`app-shell.ts`）另有 `v{version}` + 关于弹窗。
- 偏好持久化走 `lib/prefs.ts`（localStorage + try/catch 降级），现有两项
  （refresh/rateWindow）。
- e2e（xtask）playwright 段依赖行内按钮选择器；单测覆盖 program-table /
  log-viewer / a11y / prefs。
- 控制面与 WS 帧零关联：操作经 `store.programAction`（既有 `/api/programs/<name>/<action>`），
  本变更不动后端。

## Goals / Non-Goals

**Goals:**

- 一套菜单组件同时服务三入口（右键 / ⋯ / 键盘 Menu 键），内联确认状态机收敛在组件内，
  两个表面零重复逻辑。
- 日志三档配色为组件级固定配色（不进全局主题 tokens，不随 `wa-dark` 翻转）。
- e2e/单测有稳定的 data 属性钩子可依（`.op-trigger`、`.menu-item[data-kind]`）。

**Non-Goals:**

- 不改后端 API / WS 帧 / store 语义；不引入 popover/menu 第三方组件库。
- 不做日志配色自定义色板（只三档预设）。
- 不触碰「移除应用」、Apply、凭据门、树导航。
- 不为概况条网格引入新的断点数值。

## Decisions

### D1：统一菜单组件 `components/action-menu.ts`

单一 Lit 组件承载完整状态机 `closed → open → confirming(stop|restart) → busy`：
`start` 从 open 直接执行；`stop/restart` 进入 confirming（同浮层变形）；Esc / 点外 /
滚动 / resize 关闭并归还焦点。右键、⋯、键盘 Menu 键只是三种打开方式，打开后行为
完全一致——满足 spec「双入口同一菜单」。

- **定位**：浮层 `position: fixed` 渲染在组件模板末尾。祖先 `.table-scroll`
  （overflow-x: auto）不裁剪 fixed 层（无 transform/filter 包含块）；监听
  `scroll`（capture）/`resize` 关闭，规避滚动错位。
- **备选与否因**：① 各表面自带菜单逻辑（否决：状态机/反馈双份漂移，正是本轮要消除的
  不一致）；② 原生 `<menu type="context">`（否决：浏览器支持残缺）；③ Popover API
  （否决：与 version-dialog 的手写浮层模式不一致，收益不足）。
- **可访问性**：`role="menu"` / `role="menuitem"`，方向键循环聚焦，Enter 触发，Esc
  逐级返回（confirming → open → closed）；打开时焦点入首个可用项，关闭归还触发器。

### D2：入口集成

- `program-table.ts`：`renderActions` 由按钮组改为单个 `⋯` 触发钮（class
  `op-trigger`，悬停/聚焦浮现与保留列不变，`@media (hover: none)` 常显）；行上
  `contextmenu` → `preventDefault()` 并打开菜单。菜单项集合沿用 `actionsFor(state)`
  （迁入 action-menu 或由表面传入，倾向迁入菜单组件，两表面共享）。
- `program-detail.ts`：头部按钮组与 inline confirm 整体替换为一个 `⋯` 触发钮；
  详情页容器 `contextmenu` 打开同一菜单。
- busy/反馈：菜单组件内持有 busy 与结果，反馈走各表面现有的 feedback/notice 通道
  （经 `program-action-result` 自定义事件上抛，表面自行展示——保持表格级反馈条与
  详情页 notice 的现状视觉）。

### D3：动作配色

复用状态 token：启动 `--ak-status-done(-bg/-border)`、重启 `--ak-status-working*`、
停止 `--ak-status-failed*`。菜单项 = 色相前景 + `color-mix` 8–14% 底色 + 悬停加深；
确认按钮 = 对应色实底白字。动作词文字 + 图标（▶/⟳/■ 形状差异）保证灰度可辨。不新增
全局 token。

### D4：日志三档配色（组件级 CSS 变量组）

log-viewer host 上 `data-log-palette="black|white|cream"`，组件 styles 内定义三组
局部变量（不进 tokens.css——它们不参与主题系统）：

| 档位 | --log-bg | --log-fg | --log-err (stderr) | --log-gap |
|---|---|---|---|---|
| black（缺省） | `#0f1115` | `#e6e9ef` | `#f47174` | `#ff9457` |
| white | `#ffffff` | `#1f2328` | `#b42318` | `#b45309` |
| cream | `#F2EDDF` | `#4A4234` | `#a63d40` | `#8a6d3b` |

cream 档底/正文为规范钦定值（spec 锁定）；black/white 为近黑/纯白实现常量。工具栏
本身保持现有表面配色（只有日志滚动区换档），`row[data-stream='1']` 与 `.gap-row`
改引局部变量。`.empty-hint` 样式与渲染一并删除。

### D5：偏好扩展 `prefs.ts`

`LOG_STYLE_CHOICES = ['black','white','cream']`，key `ak.log_style`，缺省 `black`，
沿用 loadNumber 式校验（非法值回落缺省）与 try/catch 降级；模块头注释改写为
「三个偏好 + 各自语义」，删除「nothing else — resist feature creep」的过时表述
（新增项即本轮规范要求，非蠕变）。

### D6：概况条网格重排

删「版本」格：≥1120px 档 7 列改 6 列；720–1119px 与 ≤720px 档的
grid-template-areas 同步去掉 version 槽位。断点数值与其余六格（配置/运行/巡检/
CPU/内存/刷新）不动；「不出现控件孤跳右缘」的既有断点契约由重排后的 areas 保证。

## Risks / Trade-offs

- [右键在触屏/部分浏览器不可用] → ⋯ 按钮为主入口、右键仅快捷方式（已定决策）；
  `contextmenu` 事件不可用时无功能损失。
- [fixed 浮层在滚动/窗口变化下错位] → scroll(capture)/resize 即时关闭菜单；
  菜单打开期间不锁定滚动（关闭即回到稳定态）。
- [行 contextmenu 与行点击导航打架] → contextmenu 不产生 click；菜单打开时给行
  加 `data-menu-open` 防伪事件穿透；点外关闭判定用 `pointerdown` capture。
- [e2e 选择器全面失效] → 菜单项保留 `data-kind`、触发钮带 `.op-trigger` 与
  `data-program`，e2e 段同步重写为「右键或点 ⋯ → 点菜单项 →（确认）」。
- [localStorage 禁用环境偏好不持久] → 沿用既有 try/catch 会话内降级，行为与
  refresh/rateWindow 偏好一致。
- [确认按钮与后端拒绝竞态] → busy 期间菜单项全量禁用，请求结果经反馈通道上抛，
  菜单关闭（与现有 `window.confirm` 路径等价）。

## Migration Plan

纯前端：webui `pnpm exec tsc --noEmit && pnpm test && pnpm build` → `cargo build`
重新嵌入 dist → `cargo run -p xtask -- e2e` 全量回归。回滚 = revert 提交，无数据
迁移、无后端兼容面。旧偏好键无人读取，无需清理。
