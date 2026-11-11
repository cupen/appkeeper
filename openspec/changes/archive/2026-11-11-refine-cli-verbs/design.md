# Design: refine-cli-verbs

## Context

动词面重构（refactor-cli-verbs，已实施未归档）之后的收尾优化。四个事实基础：
守护进程周期检出已存在（supervisor `detect()` 周期 tick；apply-workflow 场景
「周期检出免手工 reload」），`ak reload` 只是「立即重扫一次」的便捷动词；
`StatusDoc.pending` 已在 `/v1` 投影与 WS 快照中；控制面已有
`/v1/apps/<app>/start|stop|restart` 端点；`ak config` 动作族为旗标形态
（`--set/--get/--delete/--edit/--init`，configuration「config 子命令」），为
重载入口提供天然归所。另：refactor-cli-verbs 与本 change 先后 MODIFIED
control-plane「CLI 控制命令」（整块替换链），归档顺序必须 refactor 先、
refine 后。

## Goals / Non-Goals

**Goals:**

- `start` 与 `stop` 对偶，`run` 专注 ad-hoc 注册（docker run 心智收窄为「跑新的」）
- pending 变更在 `ps` 表格直显；`reload` 收编为 `ak config --reload`，daemon
  配置（含 webui 热开/热关）保留唯一的 CLI 触发路径
- 控制面 `/v1` API 零增删、webui 零改动

**Non-Goals:**

- 不动 `apply|action|signal|pid|log|ps` 其余参数与 `compose --name`；不改变 app 与
  program 的两层领域模型及程序名全局唯一约束
- 不改 pending 检出/apply 的服务端语义（仅迁移 CLI 动词与提示文案）
- 不做旧动词迁移拦截（run 旧形态得到 clap 用法错误即止）

## Decisions

- **D1 管理面 `<app> [<program>]` 双位置参数**（用户拍板「语义+文案统一」）：
  `start|stop|restart` 镜像 `apply` 的范围语法，裸 `<app>` 整 app 扇出，第二位置
  参数收窄单程序；三者 `--app` 旗标移除。输出沿用原 run：逐程序
  `{程序名, 配置路径, PID}`、`--json`（单对象/扇出数组）、已 running 409、
  未知 404、离线退出码 3。备选「仅改文案、裸名仍程序级」被否——用户要求 app
  优先的自洽心智模型。
- **D2 `run` 收窄为纯 ad-hoc**（用户拍板）：positional `<name>` 与 `--app` 扇出
  删除（`start` 承接），ad-hoc 命名 `--name` 改名 `--app`；
  `reject_removed_start` 拦截器及其单测删除（`start` 重新合法）。备选「保留
  positional 别名」被否——两动词语义重叠。
- **D3 `reload` 收编为 `ak config --reload`**（用户拍板：`ak reload` 会改动
  运行态，顶层动作面不保留重载词；重读 daemon 配置属配置管理，归 config 动作族
  为第六旗标）：顶层动词与 shell `reload` 词删除；`--reload` 经既有
  `/v1/reload` 端点（client 封装复用，控制面零变化），单独使用或与写动作
  （`--set`/`--delete`/`--edit`）组合一步生效，与 `--get`/`--init` 组合为用法
  错误（退出 2）；离线退出码 3；daemon 上报重载失败非零退出；「重跑
  `ak reload`」类补救提示改为「`ak config --reload` 后 `ak apply`（或等待一个
  重扫周期）」。webui 热开启链路由此闭环——webui-api「配置驱动的控制台生命
  周期」的 `ak reload` 引用同步改写，无需给周期检出增加 webui 意图发布；顺手
  校正 configuration「按应用粒度的热更新」不可热更场景中已移除键 `port` 的
  表述为 `socket`（supervisor 现行 daemon_hints 即 socket 变更提示重启）。备选
  「彻底删 CLI 面」被否——webui 热生效与手工改配置后的立即生效将无 CLI 路径；
  备选「保留顶层 `ak reload`」被否——用户要求顶层动作面无重载词。
- **D4 `ps` PENDING 列呈现**（主 agent 定，低风险可逆）：程序行加 PENDING 列
  （`StatusDoc.pending.programs` 命中该行则标 changed/new 类标记，未命中为空）；
  `apps_added`/`apps_removed`/`daemon_hints`/`errors` 以表格尾部摘要行呈现；
  `--all` 全量视图中未加载 app 的程序行同样标记；`--out json` 零改动（投影已含）；
  渲染器与 shell `status` 共享，shell 表格随之带列。
- **D5 shell 词表与 CLI 全面对齐**（`start` 承接与「语义与对应 CLI 一致」原则
  强制；`status`→`ps` 为用户拍板）：三元组改 `start|stop|restart`（双位置参数），
  `status`/`st` 改名 `ps`，`reload` 词删除后 similar-hint 建议改为
  `pending`/`apply`。
- **D7 帮助简洁 + 三行示例**（用户要求）：全部动词/旗标帮助文案收紧为短句，
  以 app 为管理主词；仅在第二位置参数和 `pid/log/action/signal` 等进程面操作
  使用 program。顶层 `ak --help` 末尾 `after_help` 三行示例：
  `ak daemon start`（启动守护进程）、`ak start <app>`（启动应用）、
  `ak stop <app>`（停止应用）。
- **D8 app 优先但不抹平两层模型**（用户拍板）：`start/stop/restart/apply` 以
  `<app> [<program>]` 寻址；API/webui/进程面动词仍以全局唯一程序名定位；
  glossary 的「标识符不变量」同步区分两类寻址，避免把 app 误说成进程本体。
- **D6 归档顺序**：`refactor-cli-verbs` 先归档、本 change 后归档——两者对
  control-plane「CLI 控制命令」是整块替换链，本 delta 已包含 refactor 待归档的
  全部场景与本文的叠加修改，后归档者产出最终文本。实施顺序无约束（代码基于
  refactor 已推送的基线）。

## Risks / Trade-offs

- [run 旧调用（positional/`--name`）裸断为 clap 错误] → 用户知情选择；README
  迁移对照表覆盖（run <name>→start <app>、run --app→start <app>、--name→--app）
- [`ak reload` 脚本迁移] → README 迁移表指向 `ak config --reload`（语义等价）；
  等待一个重扫周期亦可达同一效果
- [PENDING 列宽挤占表格] → 标记用短词（changed/new），空值为空串
- [e2e 对 run 启动/扇出/reload 的既有断言面大] → 机械化替换 + 新断言
  （ps PENDING 列、start --json、--name 拒绝）

## Migration Plan

1. 实施（代码基于 refactor-cli-verbs 已推送基线，无冲突）
2. 全量门禁（L2：cargo test --workspace + xtask e2e --no-browser）
3. 归档顺序：`add-windows-service-registration`（Windows 真机验收后）→
   `refactor-cli-verbs` → `refine-cli-verbs`

## Open Questions

（无——D4/D5 为主 agent/一致性原则推定的呈现与词面细节，均可逆）
