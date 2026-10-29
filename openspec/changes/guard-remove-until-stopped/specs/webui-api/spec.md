# webui-api Delta: guard-remove-until-stopped

## ADDED Requirements

### Requirement: Web App 移除端点

控制台 SHALL 提供 `POST /api/apps/<app>/remove`，经与控制面 `POST /v1/apps/<app>/remove` 相同的 Supervisor 命令执行。两端点 SHALL 具有相同的前置状态检查、注册配置删除边界、成功结果字段及错误语义；Web 端点 MUST NOT 自行停止程序、直接删除注册或触发 apply。

未知 app SHALL 返回 404 错误对象；存活或待自动重启程序阻塞时 SHALL 返回 409 与阻塞程序及状态；注册删除失败 SHALL 返回服务端错误。配置了 Web 凭据时，该端点 SHALL 与现有 `/api` 写操作一样要求有效会话。成功后的状态投影与 WS 更新 SHALL 反映 app 及其程序已移除，不保留该 app 的注销 pending。

#### Scenario: Web 移除复用控制面语义

- **WHEN** 对同一已停止 app 分别使用 `/api/apps/<app>/remove` 与 `/v1/apps/<app>/remove` 的移除操作
- **THEN** 两者返回同构移除结果，产生相同的注册删除、配置保留及状态清理结果

#### Scenario: Web 移除冲突

- **WHEN** 对含 running、starting、stopping 或 backoff 程序的 app 请求 Web 移除端点
- **THEN** 返回 409 与阻塞原因，app 继续运行且注册不变，未触发 stop 或 apply

#### Scenario: Web 移除要求会话

- **WHEN** Web 配置了 auth_token，未持有效会话请求 app 移除端点
- **THEN** 返回 401，注册与程序状态均不变

#### Scenario: 移除后投影同步

- **WHEN** Web 移除成功且存在 WebSocket 订阅
- **THEN** 后续状态查询与 WS 更新均不再包含该 app/program，其他 app 的状态与 pending 不受影响
