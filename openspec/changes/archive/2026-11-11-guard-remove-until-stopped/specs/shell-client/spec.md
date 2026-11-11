# shell-client Delta: guard-remove-until-stopped

## ADDED Requirements

### Requirement: shell 应用移除

shell SHALL 支持 `remove <app>`，经控制面 app 移除端点执行，语义与 `ak remove <app>` 一致：仅允许无 `running`、`starting`、`stopping`、`backoff` 程序的 app，成功后立即删除注册并清理状态，无需 apply，且不自动停止程序。shell MUST NOT 本地直接修改注册文件。

拒绝时 SHALL 显示控制面返回的阻塞程序及状态，并提示先执行 `stop <app>`；守护不可达、未知 app 与失败的退出码遵循 shell 单命令模式的既有约定。移除成功后 shell 可继续接受命令，后续状态查询不再显示该 app。

#### Scenario: shell 中移除运行应用被拒绝

- **WHEN** 在 shell 中对含 running 程序的 web 输入 `remove web`
- **THEN** 显示冲突原因与先执行 `stop web` 的提示，web 继续运行且注册不变，shell 不退出

#### Scenario: shell 中停止后移除

- **WHEN** 在 shell 中执行 `stop web`，待全部程序停止后输入 `remove web`
- **THEN** web 的注册与状态立即清除，输出移除结果且不要求输入 apply，后续状态查询不含 web
