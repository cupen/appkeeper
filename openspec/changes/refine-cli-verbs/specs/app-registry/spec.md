# app-registry Delta: refine-cli-verbs

## MODIFIED Requirements

### Requirement: 离线可用

compose / remove MUST NOT 依赖守护进程运行（注册表查看见 `ak ps --all` 的离线降级）：守护进程离线时 SHALL 直接维护 `app_dir` 链接完成操作（离线时 MUST NOT 因无法同步而报错）；守护进程在线时 SHALL 在持久化后通过控制 API 触发同步（重扫检出），使注册变化进入待应用（pending）状态，由 `ak apply` 落地生效。在线同步失败 SHALL 视为本次命令失败：错误信息 MUST 说明注册已持久化但守护进程未同步及补救方式（等待一个重扫周期（守护进程周期性重扫，见 `apply-workflow`）后重试 `ak apply`，或执行 `ak config --reload` 触发立即重扫后重试），并以非零码退出，MUST NOT 仅以警告带过。

`ak run` 的 ad-hoc 形态为**守护进程执行**（daemon-first），MUST NOT 适用上述离线直写路径：守护进程离线时该形态 SHALL 以退出码 3 报错，MUST NOT 离线写盘或隐式拉起守护进程。

#### Scenario: 离线批量注册后启动

- **WHEN** 守护进程停止状态下依次 compose 两个应用，随后启动守护进程
- **THEN** 两个应用按 priority 与依赖关系全部自动拉起

#### Scenario: 在线同步失败不静默

- **WHEN** 守护进程在线但其重扫失败时执行 `ak compose`
- **THEN** 注册已持久化到 `app_dir`，命令以非零码退出，错误信息说明守护进程未同步并提示执行 `ak config --reload`（或等待一个重扫周期）后执行 `ak apply`

#### Scenario: 在线注册进入 pending 而非立即生效

- **WHEN** 守护进程在线时执行 `ak compose`，随后立即执行 `ak ps`
- **THEN** 注册同步成功（命令成功退出），新 app 的程序尚未运行，处于 pending；执行 `ak apply` 后按 `restart` 启动语义拉起

#### Scenario: 离线 run ad-hoc 退出 3

- **WHEN** 守护进程未运行时执行 `ak run -- ./server --app web`
- **THEN** 以退出码 3 报守护进程不可达，`app_dir` 不变，守护进程未被拉起
