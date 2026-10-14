## MODIFIED Requirements

### Requirement: daemon 根配置

daemon 配置全局唯一，SHALL 只包含全局内容：`[daemon]`（监听地址/端口、鉴权 token、日志、巡检间隔、`app_dir` 应用注册目录）、可选的 `[app-default]`（全部应用共享的默认值表，字段名与程序字段一致）与可选的 `[webui]`（内嵌控制台段：`listen` 监听地址；段存在即启用控制台，`listen` 缺省 `127.0.0.1:9877`）。daemon 配置 MUST NOT 承载任何应用专属内容：应用条目（如 `[[app]]`）、应用名、配置路径等 SHALL 被校验直接拒绝。`app_dir` 缺省为 daemon 配置文件同级的 `app.d` 目录（生产默认落位 `/etc/appkeeper/app.d`）。`daemon.log_dir` 缺省为 `/tmp/appkeeper/logs`（绝对路径，不随 daemon 配置文件位置漂移）；配置的 `log_dir` MUST 为绝对路径，相对路径 SHALL 被校验拒绝（守护进程拒绝启动）。daemon 配置默认路径 SHALL 为 Linux `/etc/appkeeper/daemon.toml`、Windows `%APPDATA%\appkeeper\daemon.toml`，并 SHALL 可用 `-c/--config` 覆盖。daemon 配置文件不存在时守护进程 SHALL 以内置默认值空启动，首次写入时创建。

#### Scenario: 全新机器空启动

- **WHEN** 在没有任何 daemon 配置文件的机器上执行 `ak daemon`
- **THEN** 守护进程以默认设置启动，受管程序为空，控制平面正常可用，控制台不伺服

#### Scenario: daemon 配置中的应用条目被拒绝

- **WHEN** daemon 配置中出现 `[[app]]` 等应用专属条目
- **THEN** 校验失败并报未知字段，守护进程拒绝以该配置启动

#### Scenario: 缺省日志目录固定

- **WHEN** daemon 配置未声明 `log_dir`，守护进程拉起任一程序并产生输出
- **THEN** 程序日志写入 `/tmp/appkeeper/logs`，与 daemon 配置文件所在目录无关

#### Scenario: app_dir 缺省名为 app.d

- **WHEN** daemon 配置未声明 `app_dir`，执行 `ak add ./proc --name abc` 完成注册
- **THEN** 注册记录写入 daemon 配置文件同级的 `app.d/abc.toml`（Linux 生产默认 `/etc/appkeeper/app.d/abc.toml`）

#### Scenario: 显式 app_dir 覆盖默认名

- **WHEN** daemon 配置显式声明 `app_dir = "apps"`
- **THEN** 注册记录写入该 `apps` 目录，缺省名 `app.d` 不生效
