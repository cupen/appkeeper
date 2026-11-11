## ADDED Requirements

### Requirement: 守护概况携带构建元数据

`/v1/status`、`/api/overview` 与 `/ws` 快照中的守护进程概况 SHALL 新增只读 `build` 对象，
包含 `git_commit`、`built_at`、`target`、`rustc` 四个字符串字段：分别为构建时的 git 提交
（短哈希，工作区不干净时带 `-dirty` 后缀）、构建时间（RFC 3339 UTC）、目标三元组与 Rust
编译器版本。任一信息在构建期不可得时，该字段 SHALL 为 `unknown`，构建 MUST NOT 因此失败。
三处投影 MUST 取自同一结构体、同源同构；未知字段对旧消费者 MUST 可安全忽略。

#### Scenario: 控制面投影含构建信息

- **WHEN** 请求 `GET /v1/status`
- **THEN** `daemon.build` 含 `git_commit`、`built_at`、`target`、`rustc` 四个非空字符串

#### Scenario: 控制台投影与 WS 同构

- **WHEN** 同一时刻读取 `GET /api/overview` 与收到 `/ws` 快照帧
- **THEN** 两者的 `daemon.build` 与 `/v1/status` 完全一致

#### Scenario: 元数据缺失不阻断构建

- **WHEN** 构建环境没有 git 仓库或无法取得编译器版本
- **THEN** 二进制仍能构建成功，对应字段返回 `unknown`
