## MODIFIED Requirements

### Requirement: webui 配置段

daemon 配置的 `[webui]` 段 SHALL 由 config 子命令按统一键表机制管理：`config --set webui.listen=<addr>` 写入监听地址（写入即开启控制台，reload 后生效或热生效）、`config --get webui.listen` 打印生效值（段未配置时 SHALL 回退内置默认 `127.0.0.1:9877`）、`config --delete webui` 删除整段（即关闭控制台）。键表机制、类型校验、读改写与原子落盘 SHALL 与 `[daemon]` 键一致（见 config 子命令相关 requirement）。`webui.listen` 值 SHALL 为非空的 `host:port` 形式：无法解析或为空 SHALL 被校验拒绝（写入期与加载期一致拒绝）。

`[webui]` 段 SHALL 另含 `auth_token` 键，其生效值 SHALL 为 `auto` 或非空固定 token：

- 未配置该键时生效值 SHALL 为保留值 `auto`：守护进程启动时随机生成 token（生成、保存与轮换规则见 `webui-api`「配置驱动的控制台生命周期」）。`config --get webui.auth_token` 在未配置时 SHALL 打印 `auto`（生效值语义，与 `webui.listen` 回退默认一致）。
- 显式写入 `auth_token = "auto"` SHALL 与未配置等价；小写 `auto` 为保留关键字，不能作为固定 token 字面量。
- 其它非空字符串 SHALL 按固定 token 使用；空值 SHALL 被校验拒绝。
- 控制台不存在免鉴权模式：开启控制台即要求凭据（鉴权语义见 `webui-api` 能力规范），因此 `ak validate` MUST NOT 再输出「控制台缺少 auth_token」类警告。

控制台 SHALL 保留 TCP 监听，MUST NOT 因控制面改用 socket 而要求浏览器经本地 IPC 端点访问。`[webui]` 段内未知键 SHALL 被校验拒绝。

#### Scenario: config 开启控制台

- **WHEN** 执行 `ak config --set webui.listen=127.0.0.1:9877`（daemon 离线）
- **THEN** daemon 配置文件新增 `[webui]` 段且整体校验通过，输出说明离线时下次启动生效

#### Scenario: config 关闭控制台

- **WHEN** 配置含 `[webui]` 段，执行 `ak config --delete webui`
- **THEN** 该段整体从文件移除（其余内容含注释保持原样），守护在线时输出需 reload 提示

#### Scenario: 非法 listen 拒绝写入

- **WHEN** 执行 `ak config --set webui.listen=9877`（缺 host）或 `ak config --set webui.listen=`（空值）
- **THEN** 非零码退出并说明值必须是 `host:port`，配置文件内容不变

#### Scenario: 段内未知键校验拒绝

- **WHEN** daemon 配置的 `[webui]` 段内出现未知键（如 `auth = true`），执行 `ak validate` 或 `ak daemon start`
- **THEN** 校验失败并指明该未知字段，守护进程拒绝以该配置启动

#### Scenario: 配置控制台 token

- **WHEN** 执行 `ak config --set webui.auth_token=<token>`
- **THEN** `[webui]` 段新增 `auth_token` 键且整体校验通过；控制台 reload 后要求该固定 token

#### Scenario: 未配置 token 的生效值为 auto

- **WHEN** `[webui]` 段存在但未写 `auth_token`，执行 `ak config --get webui.auth_token`
- **THEN** 打印 `auto`，配置文件不被改写

#### Scenario: 显式 auto 等价于默认

- **WHEN** 执行 `ak config --set webui.auth_token=auto`，随后启动守护进程
- **THEN** 校验通过，控制台以 auto 模式伺服（启动时随机生成 token）

#### Scenario: 空 webui token 被拒绝

- **WHEN** 执行 `ak config --set webui.auth_token=`
- **THEN** 非零码退出并说明值必须为 `auto` 或非空字符串，配置文件内容不变

#### Scenario: 未鉴权警告不再出现

- **WHEN** `[webui]` 段存在且未写 `auth_token`，执行 `ak validate`
- **THEN** 校验通过且不输出「控制台缺少 auth_token / 任何本地用户可访问」类警告
