## ADDED Requirements

### Requirement: open 与控制台引导 token 一致

shell 的 `open` 命令 SHALL 与 `ak webui` 共用同一控制台打开实现：经控制面 `GET /v1/webui` 读取控制台状态与（`auth == "auto"` 时的）引导 token，探测可达性后调用系统默认浏览器。auto 模式下 URL SHALL 携带 `?token=<token>`；固定 token 模式 MUST NOT 携带。控制台不可达时 shell SHALL 维持既有提示（含启用方式）且不启动浏览器，shell 不退出。该命令 MUST NOT 修改配置或启停受管程序。

#### Scenario: auto 模式下 open 携带引导 token

- **WHEN** 控制台以 auto 模式可达，shell 中输入 `open`
- **THEN** 浏览器打开的控制台地址含 `?token=<token>`，shell 不退出

#### Scenario: 固定 token 模式不带 token

- **WHEN** 控制台配置了固定 `auth_token`，shell 中输入 `open`
- **THEN** 打开的控制台地址不含 `?token=`，shell 不退出

#### Scenario: open 在控制台未启动时

- **WHEN** 控制台未伺服且 shell 中输入 `open`
- **THEN** 输出"webui 未启动"的提示（含启用方式），不启动浏览器，shell 不退出
