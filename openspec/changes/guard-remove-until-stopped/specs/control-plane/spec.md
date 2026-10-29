# control-plane Delta: guard-remove-until-stopped

## ADDED Requirements

### Requirement: App 移除控制端点

控制面 SHALL 提供 `POST /v1/apps/<app>/remove`，供 CLI、shell 与其他控制客户端请求 app 级注销。端点 SHALL 经守护进程的命令队列执行 app-registry 定义的无存活进程前置检查、注册删除与状态清理，MUST NOT 由客户端或 HTTP 连接线程自行停止进程或维护注册文件。

未知 app SHALL 返回 404；被删除 app 中任一程序处于 `running`、`starting`、`stopping` 或 `backoff` 时 SHALL 返回 409，响应列出阻塞程序及状态；成功 SHALL 返回结构化结果，至少含 app 名、注册记录路径、是否删除生成文件及保留的外部配置路径（适用时），结果 SHALL 表明移除已生效且无需 apply。端点在本地 IPC 与 TCP 形态下的准入规则和响应语义 SHALL 一致。

`ak remove <app>` SHALL 调用该端点，守护不可达时输出提示并以退出码 3 退出；CLI MUST NOT 隐式拉起守护进程，也不得在请求后另行触发 reload 或 apply。其他控制错误的退出码保持 CLI 既有约定。

#### Scenario: 存活程序导致移除冲突

- **WHEN** 对含 running 程序的 web 请求 `POST /v1/apps/web/remove`
- **THEN** 返回 409，错误列出阻塞程序及状态；web 的状态与注册记录不变，其他程序也未被停止

#### Scenario: 已停止应用立即移除

- **WHEN** 对全部程序已停止的 web 请求 `POST /v1/apps/web/remove`
- **THEN** 返回包含 app 与配置路径的结构化成功结果，后续状态投影不再含 web，注册记录已删除且没有该 app 的注销 pending

#### Scenario: 未知应用返回 404

- **WHEN** 请求 `POST /v1/apps/not-registered/remove`
- **THEN** 返回 404，注册表与运行状态不变

#### Scenario: CLI 离线不移除

- **WHEN** 守护进程不可达时执行 `ak remove web`
- **THEN** CLI 输出不可达提示并以退出码 3 退出，不直接删除注册、不自动拉起守护进程，也不请求 apply
