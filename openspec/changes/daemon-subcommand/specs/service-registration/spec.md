# service-registration 变更增量：daemon-subcommand

## MODIFIED Requirements

### Requirement: service install 生成并注册 systemd unit

`ak service install` SHALL 在 `/etc/systemd/system/ak.service` 生成 unit 文件，并执行 `systemctl daemon-reload` 与 `systemctl enable`，使服务开机自启。unit 文件 MUST 满足：

- `ExecStart` 为当前 appkeeper 可执行文件的绝对路径，后接 `daemon --config <daemon 配置文件绝对路径>`（daemon 配置路径按 `-c/--config` 解析后的路径；未指定时用平台默认路径）。
- `Restart=always` 与 `RestartSec`，崩溃退出后由 systemd 重新拉起。
- 优雅停机走 SIGTERM（与 appkeeper 自身信号处理一致），并配置 `TimeoutStopSec` 不小于配置解析后的最长停止等待。

升级兼容：既有 unit 的 `ExecStart` 若仍是旧入口形态（`run --config`），在新二进制下 systemd 拉起 SHALL 被守护形态拦截逻辑拒绝（退出码 2）；拦截错误信息 SHALL 提示以 `ak service install --force --now` 重新生成 unit。`ak service install` 自身的 `--force` 覆盖语义保持不变。

#### Scenario: 安装成功并开机自启

- **WHEN** 用户以 root 执行 `ak service install`
- **THEN** `/etc/systemd/system/ak.service` 存在且 ExecStart 指向当前可执行文件与解析后的 daemon 配置路径（`daemon --config <路径>` 形态），`systemctl is-enabled appkeeper` 返回 enabled

#### Scenario: --now 立即启动

- **WHEN** 用户执行 `ak service install --now`
- **THEN** 注册完成后执行 `systemctl start appkeeper`，服务进入 running 状态

#### Scenario: 旧 unit 升级后拉起失败并给自救提示

- **WHEN** 既有 unit 的 ExecStart 为旧入口形态 `run --config <路径>`，二进制升级为移除守护形态 `run` 的版本后 systemd 拉起服务
- **THEN** 服务启动失败（拦截退出码 2），错误信息提示改用 `ak daemon` 并以 `ak service install --force --now` 重新生成 unit
