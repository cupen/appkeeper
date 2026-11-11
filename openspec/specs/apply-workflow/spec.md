# apply-workflow Specification

## Purpose

定义配置变更的两阶段生效机制：守护进程检出（detect）磁盘配置与运行配置的
差异形成待应用（pending）变更，用户以 `ak apply` 显式触发应用
（apply），并约束范围选择、`--restart` 重启语义与结果呈现。

## Requirements

### Requirement: pending 检出

守护进程 SHALL 周期性重扫 `app_dir` 注册并重读 daemon 配置与全部 app 配置
本体，将差异记为待应用（pending）变更，MUST NOT 因此停止、重启或拉起任何
程序。检出 SHALL 复用既有比对规则：程序按配置定义比对（内容
变化 = changed）、注册新增/移除按 app 归属记录。跨应用校验失败时 SHALL
整体不形成 pending 并上报错误；单个 app 文件
非法时 SHALL 隔离——该 app 不进入 pending、保持旧定义，其余 app 照常检出。
pending 的用户可见面为 `ak ps` 的 PENDING 列与表格尾部摘要（见 `control-plane`
能力规范）；立即重扫 SHALL 经 `/v1/reload` 端点触发，CLI 入口为
`ak config --reload`（顶层 `reload` 动词已移除，见 `configuration`；周期检出使
立即重扫仅为便捷入口而非必需）。

#### Scenario: 配置修改只进入 pending

- **WHEN** 守护进程运行中修改某 app 的程序 command 字段，等待一个重扫周期
- **THEN** 该程序在 pending 中标记为 changed，但进程未被停止或重启，状态与 pid 不变

#### Scenario: 周期检出免手工 reload

- **WHEN** 直接编辑磁盘上的 app 配置文件（未执行任何命令），等待一个重扫周期
- **THEN** 守护进程无需任何手工触发即形成 pending 变更

#### Scenario: 单个 app 文件损坏不影响其他检出

- **WHEN** 重扫时 app B 的配置存在语法错误，app A 的配置正常且已修改
- **THEN** A 的变更进入 pending，B 不进入 pending 且保持旧定义继续运行，错误被上报

#### Scenario: 跨应用校验失败不形成 pending

- **WHEN** 重扫后的配置集存在程序重名等跨应用校验失败
- **THEN** 本轮不形成任何 pending，运行状态不变，错误被上报；修复后下轮重扫恢复正常

### Requirement: apply 范围与幂等

`ak apply` SHALL 应用 pending 变更，范围 SHALL 可指定：缺省为全部；字面量 `all` SHALL 为全量关键字，`apply all` 与缺省全量严格等价（`all` 为保留字，MUST NOT 被解析为 app 名，帮助与文档 MUST 注明 `all` 不可用作 app 名）；`apply all` 后 MUST NOT 再接受 program 参数——`apply all <program>` 形状 SHALL 报错，MUST NOT 静默忽略 program 部分而按全量执行；`apply <app>` 仅作用于该 app 的程序与该 app 的注册变化，MUST NOT 影响其他 app（其他 app 的 pending 保持不变）；`apply <app> <program>` 进一步收窄到单程序。范围内无 pending 时 SHALL 什么都不做，输出明确的「无变更」结论并以 0 退出（幂等）。新注册 app 的程序进入 apply 范围时 SHALL 按 `restart` 策略的启动语义处理：`no` 仅完成注册不启动；其余策略启动（`unless-stopped` 存在「用户停止」记录时保持停止）。

对于手工删除配置形成的 app/program 移除，apply MUST 在修改范围内任何运行状态之前检查被删除对象。若范围内任何被删除 app/program 处于 `running`、`starting`、`stopping` 或 `backoff`，本次 apply SHALL 整批拒绝并以冲突错误报告全部阻塞对象，提示先停止对应 app/program；范围内其他新增、修改、删除与 `--restart` 动作 MUST NOT 执行，已加载运行状态不变。磁盘上已经删除的配置 MUST NOT 被恢复，后续检出 SHALL 保留该删除 pending。`stopped`、`exited`、`fatal` 不阻塞删除，MUST NOT 要求曾显式执行 stop。范围外的删除不阻止本次 apply，`--restart` 不豁免该前置检查。

#### Scenario: 无变更时 apply 幂等

- **WHEN** 配置无任何 pending 时执行 `ak apply`
- **THEN** 无程序被停止/重启/启动，输出「无变更」类提示，退出码 0

#### Scenario: apply all 等价全量

- **WHEN** app A 与 app B 均有 pending 变更，执行 `ak apply all`
- **THEN** A 与 B 的全部 pending 被应用，结果与裸 `ak apply` 一致

#### Scenario: 单 app 范围不波及其他

- **WHEN** app A 与 app B 均有 pending 变更，执行 `ak apply A`
- **THEN** 仅 A 的变更被应用；B 的 pending 保留，B 的进程不受影响

#### Scenario: 新注册 app 随 apply 启动

- **WHEN** `ak compose` 注册了 `restart = "always"` 的新 app 后执行 `ak apply`
- **THEN** 该 app 的程序被启动，出现在 apply 结果中

#### Scenario: 新注册 no 策略程序只注册不启动

- **WHEN** 注册的新 app 声明 `restart = "no"` 后执行 `ak apply`
- **THEN** 注册落地（链接/生成文件就位），程序保持 `stopped`，出现在 apply 结果的未启动说明中

#### Scenario: apply 后 pending 清空

- **WHEN** 全量 `ak apply` 成功后立即查询 pending
- **THEN** pending 为空（应用失败被隔离的 app 除外，其 pending 保留）

#### Scenario: 手工删除存活 app 时整批拒绝

- **WHEN** 手工删除 running 的 app A 注册，且范围内 app B 也有 pending 修改，执行覆盖两者的 apply
- **THEN** apply 返回冲突并列出 A 的阻塞程序与状态，A 未被停止，B 的修改未应用，范围内其他程序也未被重启或启动；磁盘不恢复 A 的注册，A/B 的 pending 均保留

#### Scenario: 手工删除存活 program 时整批拒绝

- **WHEN** 从 app A 配置中删除处于 starting、stopping 或 backoff 的 program 定义，执行包含该删除的 apply
- **THEN** apply 在任何停止、删除、重建或 `--restart` 之前整批拒绝，提示先停止该程序，后续检出仍报告该 program 删除 pending

#### Scenario: 已停止的手工删除可由 apply 生效

- **WHEN** 被手工删除的 app/program 均处于 stopped、exited 或 fatal，执行覆盖它们的 apply
- **THEN** 删除按既有范围规则生效，运行投影清理，pending 清除；其他合法变更正常应用

#### Scenario: 范围外删除不阻塞

- **WHEN** app A 的存活程序被手工删除且不在本次范围内，用户对无冲突的 app B 执行 `ak apply B`
- **THEN** B 的合法变更正常应用，A 的状态与 pending 不变

#### Scenario: restart 旗标不豁免删除守卫

- **WHEN** 范围内包含 running 的手工删除对象并执行 `ak apply --restart`
- **THEN** apply 仍整批拒绝，既未删除对象也未重启任何范围内程序

### Requirement: apply 的重启语义

apply 应用配置变更时 SHALL 沿用 reload 的既有程序重建规则：变更程序停止后
以新定义重建，原先在跑则重新拉起，原先停止则保持停止。`--restart` SHALL
对范围内无配置变更的程序同样执行重启，且 MUST 遵守：手动停止的程序
（stopped / exited / fatal）保持停止，因崩溃等待退避重试的程序（backoff）
重新拉起。

#### Scenario: 变更程序原先在跑则重启

- **WHEN** 运行中的程序 `web` 的配置有 pending 变更，执行 `ak apply`
- **THEN** `web` 被停止并以新定义重新拉起，结果中 action 为重启类

#### Scenario: 变更程序原先停止则保持停止

- **WHEN** 已被 `ak stop` 停止的程序 `web` 的配置有 pending 变更，执行 `ak apply`
- **THEN** `web` 以新定义重建但保持停止，结果中 action 为仅更新定义

#### Scenario: --restart 跳过手动停止的程序

- **WHEN** 程序 `web` 无配置变更且处于 stopped、`job` 无配置变更且处于
  running、`worker` 处于 backoff（崩溃等待重试），执行 `ak apply --restart`
- **THEN** `job` 被重启，`worker` 被拉起，`web` 保持 stopped，三者都在结果中
  可见且动作不同

#### Scenario: --restart 无 pending 也重启

- **WHEN** 配置无任何 pending，执行 `ak apply --restart`
- **THEN** 范围内运行中与 backoff 的程序被重启/拉起，手动停止的保持停止

### Requirement: apply 结果呈现

apply SHALL 产出结构化结果：每个范围内程序一个条目，含程序名、所属 app、
配置是否有变化（changed）、执行的动作（如 update-and-restart /
update-only / restart / start / keep-stopped / none）、动作结果；app 级
注册变化（added / removed）SHALL 单独列出。CLI SHALL 以简洁表格或分组
清单呈现「配置变化 / 重启 / 未动」三类信息；`/v1/apply` SHALL 返回等价
JSON。结果呈现 MUST 使用户无需比对配置即可分辨每个程序发生了什么。

#### Scenario: 结果区分三类程序

- **WHEN** apply 范围内含「配置有变且在跑」「配置有变但手动停止」「无变化
  且在跑」三种程序
- **THEN** 输出逐程序标明各自的变化与动作，三类不混淆

#### Scenario: API 结果结构化

- **WHEN** 向运行中的守护进程请求 `POST /v1/apply`
- **THEN** 返回 JSON，含逐程序的 changed、action、result 字段与 app 级
  added/removed 清单
