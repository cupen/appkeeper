## Why

控制台上线后视觉与排版一直「歪歪扭扭」：状态徽章组件从未被任何视图 import（`<ak-status-badge>`
实测 0×0，App 概况「状态」整列空白、总览与详情页没有状态词），顶栏与页面内容左右各差 20px，
长应用名在 220px 侧栏内换行并把行高与计数挤歪，Unicode 状态符号与文字基线不齐且与徽章 glyph
重复。这是缺陷与视觉语言长期漂移叠加的结果，已影响控制台「一眼看清谁挂了」的核心用途。

## What Changes

- **修硬缺陷**：注册 `status-badge`（三处视图统一渲染状态徽章）；统一内容列与顶栏的左右边界；
  长名称截断不换行；去掉行内重复 glyph；表头/数字列对齐；窄屏无横向溢出。
- **信息架构重做**：
  - 总览页从「与侧栏重复的 app 卡片目录」改为**全局程序总表**（按 app 分组、失败优先），
    顶部守护进程概况保持页面级覆盖全部应用；
  - App 概况页收敛为该 app 的程序表 + app 级 apply / 移除操作；
  - 进程详情改为**单列信息流**：面包屑 + 状态 + 操作 → 紧凑指标条 → 命令行 mono 块 →
    日志占满剩余高度；
  - 「速率窗口」从页面标题移入程序表标题栏，「刷新频率」留在顶栏，顶栏固定单行、右侧对齐。
- **视觉系统收敛**：以「精致暗色 DevTool」为基准重写 token 与共用样式——石墨中性底 + 单一靛蓝
  强调色；状态只通过**小圆点 + 形状 + 中文状态词**表达（侧栏文字保持中性色）；字号/间距/
  圆角收敛为固定档位；表格、卡片、工具条、提示条共用同一套节奏。
- **交互**：列表行 hover/focus 显现 start/stop/restart 快捷操作（破坏性操作仍需确认），详情页
  保留完整操作与反馈；状态词中文化（`running → 运行中` 等），`data-status` 仍保留英文 slug。
- **BREAKING**：总览页内容与详情页结构改变；依赖旧 DOM 结构/类名的 webui 单测与 xtask e2e
  脚本需同步更新（`/v1`、`/api`、WS 协议与后端 projection 不变）。

## Capabilities

### New Capabilities

- `webui-visual-system`: 控制台的设计令牌与几何不变量——排版/间距/圆角档位、内容列对齐、
  长文本与窄屏溢出规则、状态呈现的形状与颜色分工。作为「不歪」的可验收契约。

### Modified Capabilities

- `webui-ui`: 总览页改为全局程序总表；进程详情改为单列布局；状态徽章在三处真实渲染并使用
  中文状态词；列表行快捷操作；速率窗口/刷新频率的归属与顶栏单行结构。

## Impact

- 前端：`webui/src/styles/tokens.css`、`app.css`、`shared.ts`；`app-shell.ts`；组件
  `status-badge.ts`、`console-tree.ts`、`overview-bar.ts`、`log-viewer.ts`（工具条样式）；
  视图 `dashboard.ts`、`app-overview.ts`、`program-detail.ts`；可能新增共用布局/图标模块。
- 测试：上述组件的 vitest 单测与快照断言；`xtask/src/e2e.rs` 中驱动真实浏览器的 webui 脚本
  依赖的 DOM 选择器与文案。
- 构建：前端改动后须 `pnpm exec tsc --noEmit && pnpm test && pnpm build` 再 `cargo build`
  重新嵌入 `dist/`。
- 不变：Rust 控制面/Web API/WS 帧结构与状态 projection；`/v1` 与 `/api` 语义；CLI。
