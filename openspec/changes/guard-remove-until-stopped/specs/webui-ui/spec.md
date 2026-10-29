# webui-ui Delta: guard-remove-until-stopped

## ADDED Requirements

### Requirement: App 移除控制

App 概况 SHALL 提供整个 app 的移除操作，调用 Web App 移除端点。该操作 MUST 经确认步骤后才发送请求，确认内容 SHALL 表明会删除注册、`app_dir` 内生成的配置会一并删除而外部部署配置保留，并明确不会自动停止存活程序。界面 MUST NOT 以先 stop 再 remove 的组合请求伪装成独立移除。

移除结果 SHALL 以可见反馈呈现：409 时显示阻塞程序及状态和先停止的提示，保留当前 app 与导航；成功后该 app 及其程序 SHALL 从侧栏与概况中消失，若正在查看该 app 则导航到仍可用的概况，不得继续显示可交互的陈旧控制项。成功反馈 SHALL 表明无需再执行 apply。

#### Scenario: 移除运行应用需先停止

- **WHEN** 用户对含 running 程序的 app 点击移除并确认
- **THEN** 界面显示后端返回的冲突原因及先停止提示，app 仍运行且保留在导航中，未代为执行 stop 或 apply

#### Scenario: 取消移除无副作用

- **WHEN** 用户点击移除后在确认步骤取消
- **THEN** 不发出移除请求，程序、注册与当前页面均不变

#### Scenario: 停止后从 Web 一步移除

- **WHEN** 用户在 app 全部程序停止后确认移除
- **THEN** 界面提示移除成功且无需 apply，侧栏与主区不再提供该已删除 app 的陈旧操作，若原页面选中该 app 则切换至可用概况
