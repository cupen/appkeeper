## 1. 日志窗口：配色三档与空态清理

- [x] 1.1 `lib/prefs.ts` 新增日志配色偏好：`LOG_STYLE_CHOICES = ['black','white','cream']`、
  key `ak.log_style`、`loadLogStyle()/saveLogStyle()`，非法值回落 `black`；改写模块头
  注释为三项偏好清单。验证：`prefs.test.ts` 新增用例（合法值往返、非法值回落、
  localStorage 抛异常降级）通过
- [x] 1.2 `components/log-viewer.ts`：host 增加 `data-log-palette` 属性（缺省 `black`），
  组件 styles 定义三组局部变量（black `#0f1115/#e6e9ef`、white `#ffffff/#1f2328`、
  cream `#F2EDDF/#4A4234` 及各自 stderr/gap 色，见 design D4），滚动区、`.row[data-stream='1']`、
  `.gap-row` 改引局部变量；工具栏新增档位选择器（三选一，切换即生效并持久化）。
  验证：`log-viewer.test.ts` 新增三档切换/缺省为黑用例通过
- [x] 1.3 `log-viewer.ts` 删除 `empty-hint` 渲染与样式（空输出不出现任何占位文案），
  行数计数保留。验证：测试断言空 program 视图无「等待输出」文本；`pnpm exec tsc --noEmit` 通过
- [x] 1.4 主题隔离验证：切换 `wa-dark` 类时日志区配色不随主题翻转。验证：
  `log-viewer.test.ts` 增加「切 wa-dark 后滚动区配色变量不变」断言（或组件级等价
  测试）通过

## 2. 程序操作菜单组件

- [x] 2.1 新建 `components/action-menu.ts`：状态机 `closed → open → confirming(stop|restart)`
  + busy；`actionsFor(state)` 状态机迁入（running/starting→停止+重启，stopping→空，
  其余→启动）；`start` 选中即执行，`stop/restart` 同浮层内联确认（含程序名与带
  动作色确认按钮）；Esc 逐级返回（confirming→open→closed）并归还焦点；浮层
  `position: fixed`，监听 scroll(capture)/resize/pointerdown(点外) 关闭；菜单项暴露
  `.menu-item[data-kind]`。验证：新建 `action-menu.test.ts` 覆盖状态机全迁移路径
  （start 直发、stop/restart 确认、取消零副作用、busy 禁用）通过
- [x] 2.2 菜单可访问性与配色：`role="menu"/"menuitem"`、方向键循环聚焦、Enter 触发、
  可访问名称；动作配色启动绿/重启琥珀/停止红（复用 `--ak-status-done/working/failed*`
  token，design D3），图标 ▶/⟳/■ 与动作词并存。验证：`action-menu.test.ts` 键盘路径
  与 `a11y.test.ts` 菜单角色/名称断言通过
- [x] 2.3 操作结果上抛：`program-action-result` 自定义事件（ok/text）由表面消费展示。
  验证：单测中 mock store 断言事件负载与失败路径文案

## 3. 表面集成（表格行 + 进程详情）

- [x] 3.1 `components/program-table.ts`：`renderActions` 改为单个 `⋯` 触发钮
  （`.op-trigger`、`data-program`，悬停/聚焦浮现与保留列不变、`(hover:none)` 常显）；
  行 `contextmenu` → `preventDefault()` + 打开菜单；移除 `window.confirm` 与
  DESTRUCTIVE 旧路径。验证：`program-table.test.ts` 改写为「点 ⋯/右键 → 菜单项 →
  确认」路径 + 「悬停不挤压其他列」既有断言保留通过
- [x] 3.2 `views/program-detail.ts`：头部按钮组与 inline confirm 替换为 `⋯` 触发钮；
  详情页容器 `contextmenu` 打开同一菜单；notice 反馈通道接 `program-action-result`。
  验证：程序详情相关测试改写后通过；页面无常驻 start/stop/restart 按钮组断言通过
- [x] 3.3 反馈回归：两个表面的成功/失败反馈展示与既有视觉一致（表格级反馈条 /
  详情页 notice）。验证：手工或组件测试覆盖失败原因展示（后端拒绝时不静默）

## 4. 概况条去版本号

- [x] 4.1 `components/overview-bar.ts`：删除「版本」格与 `.version` 样式，三档断点
  （1120/720px）grid-template-areas/列重排，其余六格与断点数值不动。验证：渲染断言
  无「版本」格、六格齐全；`pnpm exec tsc --noEmit` 通过

## 5. 测试与验收收口

- [x] 5.1 webui 三连绿：`pnpm exec tsc --noEmit && pnpm test && pnpm build` 全部通过，
  无残留对旧按钮/旧空态文案的引用（`grep -rn "等待输出\|window.confirm" webui/src` 仅余
  白名单处）
- [x] 5.2 xtask e2e playwright 段更新：操作交互改为「点 ⋯（或右键行）→ 菜单项 →
  （内联确认）」，选择器用 `.op-trigger` / `.menu-item[data-kind]`；验证：
  `cargo run -p xtask -- e2e`（含浏览器段）全绿
- [x] 5.3 构建嵌入与冒烟：`cargo build --release` 后起 daemon（含 `[webui]`），
  人工/脚本核对 spec 验收点——缺省黑底白字、三档切换持久、空态无文案、概况条无版本号、
  右键与 ⋯ 同一菜单、start 免确认、stop/restart 内联确认、Esc 可退出
