## MODIFIED Requirements

### Requirement: 字段优先级

同一字段 SHALL 按以下优先级取值（从高到低）：`[program.*]` 显式字段 > app 配置 `[app]` 表（注册微调 flag 落点，部署人可手改）> daemon `[app-default]` > 内置默认。`priority` 为应用级字段，仅存在于 `[app]` 与 `[app-default]` 层；`restart`（取代既有 `autorestart` 与 `autostart` 两个旧字段）为程序级字段，可在 `[app]` 与 `[app-default]` 层作应用级默认值。

#### Scenario: app 层默认生效

- **WHEN** `[app]` 设置 `restart = "on-failure"`，程序未显式声明
- **THEN** 该应用内程序按 on-failure 运行

#### Scenario: daemon 默认兜底

- **WHEN** daemon `[app-default]` 设 `restart_backoff = 2.0`，app 配置未在任何层级声明
- **THEN** 程序按 2 秒重启间隔运行

#### Scenario: 程序显式字段最高

- **WHEN** 某程序显式声明 `restart = "no"`，而 `[app]` 与 `[app-default]` 均为 always
- **THEN** 该程序按 no 运行，同应用其他程序不受影响

## ADDED Requirements

### Requirement: restart 字段域与旧字段兼容

`restart` SHALL 是唯一的启动/重启策略字段（docker 字面量语义），值域为字符串 `no` | `on-failure` | `always` | `unless-stopped`：

- `no`：退出后不重启；守护进程启动（及 apply 引入新程序）时不自动拉起。
- `on-failure`：退出码不在 `exit_codes` 期望列表（默认仅 0）时重启；守护进程启动时拉起。
- `always`：无论退出码如何都重启；守护进程启动时无条件拉起（不受用户停止记录影响）。
- `unless-stopped`：无论退出码如何都重启；守护进程启动时若无「用户停止」持久化记录则拉起，有记录则保持停止（记录的写入与清除见 process-management 能力的重启策略 requirement）。

解析 SHALL 接受宽容写法：布尔 `false` 等价 `"no"`；四层级联到底均未设置（留空）时 SHALL 取内置默认 `"no"`。守护语义的开箱体验由模板引导：`xkeeper config --init` 生成的 `[app-default]` SHALL 显式写入 `restart = "always"`，仓库随附的 daemon 配置模板同步。

旧字段兼容（弃用别名，仅读取）：`autorestart`（`never` 映射 `no`，`on-failure`/`always` 原值）与 `autostart`（布尔，应用级）SHALL 继续可读，读取时按以下规则归并为 `restart`：`autostart = false` → `no`；`autostart = true`（或未设置）且 `autorestart = X` → `X` 的映射值；`autostart = true` 且 `autorestart = "never"` → `no`，并 SHALL 输出迁移警告（该组合的「守护启动拉起」行为在新模型中不存在）。同一层级同时显式出现新旧字段 SHALL 被校验拒绝（歧义，指明二者不得混用）。所有写入路径（add 微调 flag、run ad-hoc、脚手架生成、模板）SHALL 只写新字段 `restart`。

#### Scenario: false 与留空等价 no

- **WHEN** 程序声明 `restart = false`，另一程序未声明 `restart` 且各级层级均未设置
- **THEN** 两个程序均按 `no` 运行：退出不重启，守护进程启动不拉起

#### Scenario: 新旧字段同现被拒绝

- **WHEN** 同一程序同时声明 `restart = "always"` 与 `autorestart = "on-failure"`
- **THEN** 校验失败，错误指明新旧字段不得混用，配置被拒绝

#### Scenario: 旧字段组合迁移告警

- **WHEN** 既有 app 配置声明 `autostart = true` 与 `autorestart = "never"`
- **THEN** 按 `restart = "no"` 读取并输出迁移警告，说明「守护启动拉起 + 不重启」组合在新模型中不存在

#### Scenario: init 模板保持开箱守护

- **WHEN** 执行 `xkeeper config --init` 后以模板默认值注册并启动程序
- **THEN** 模板 `[app-default]` 的 `restart = "always"` 生效，程序崩溃自动拉起、守护重启自动恢复
