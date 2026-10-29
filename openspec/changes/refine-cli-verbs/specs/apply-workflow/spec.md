# apply-workflow Delta: refine-cli-verbs

## MODIFIED Requirements

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
