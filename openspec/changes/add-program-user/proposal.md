## Why

appkeeper 目前只能以守护进程自身身份拉起子进程：daemon 以 root 跑时，业务进程也全部是
root，权限过大；daemon 以普通用户跑时，又无法托管需要专用账号的服务。Linux 侧的
systemd 早已用 `User=` 解决这件事，而 appkeeper 的 app 配置里没有等价字段。补上
`[program.<name>].user`，让每个程序能声明自己的运行身份，缺省继承 daemon 身份、并在
落到 root 时给出警告。

## What Changes

- **配置新增 `[program.<name>].user`**（仅 Unix）：用户名或数字 uid；缺省不写 =
  继承 daemon 的有效用户（按 `getpwuid(geteuid())` 解析，不读 `$USER` 环境变量，也
  不把用户名写进配置文件）。
- **解析期校验**：用户在系统中不存在 SHALL 报错（`ak validate` / compose 预检即可发现）；
  Windows 平台出现该字段 SHALL 明确报错。
- **执行期拦截**：daemon 非 root 且目标 uid 与 daemon euid 不同 SHALL 在提交前拒绝
  （compose 预检 400、零副作用），daemon 启动加载时剔除该 app 并 warn，MUST NOT 掉进
  spawn 失败的重启退避循环。
- **降权实现**：euid 与目标一致时不降权；euid 为 root 时在 `pre_exec` 中按
  `setgroups -> setgid -> setuid` 顺序降权，附加组在配置解析期预取（`getgrouplist`）。
- **环境修正**：切到目标用户后，`HOME`/`USER`/`LOGNAME`/`SHELL` 未在
  `[program.x.env]` 显式设置时 SHALL 注入 passwd 记录值，避免沿用 root 的环境。
- **配置新增 `[program.<name>].group`**（仅 Unix）：主组名或数字 gid；缺省取目标用户的
  passwd 主组，可单独使用（只换组、不换用户）。非 root 且目标用户与 daemon 相同时，
  换组 SHALL 仅允许切到 daemon 已属的组，否则在执行前拒绝。
- **配置新增 `[program.<name>].umask`**（仅 Unix）：八进制字符串（如 `"0027"`）；
  仅影响子进程创建的文件，由 `pre_exec` 的 `umask(2)` 落地，MUST NOT 影响 daemon 自身
  写入的日志文件；缺省继承 daemon 的 umask。
- **root 警告**：最终运行身份为 uid 0（缺省继承或显式 `user = "root"`）SHALL 产生
  非致命警告，在 daemon 日志与 `ak compose up` 预览输出中可见。
- **变更检测**：`user` 进入 `ResolvedProgram` 定义哈希，修改后 reload/apply 自动重启。
- 不在本次范围：`ak run` 的 ad-hoc `--user`、`ak ps` 投影新增 user 列、health_check /
  自定义 action 以目标用户执行（仍以 daemon 身份执行，文档写明）。

## Capabilities

### New Capabilities

（无）

### Modified Capabilities

- `configuration`: 程序字段表新增 `user`，定义取名规则、缺省语义与校验失败行为。
- `process-management`: 子进程拉起新增身份降权契约（降权顺序、附加组、环境变量修正、
  不可行时在执行前拦截而非重试）。
- `control-plane`: compose up 预览返回非致命警告列表；不可行的 `user` 归入 400
  validation（零副作用）。

## Impact

- **代码**：`src/config.rs`（`ProgramRaw`/`ResolvedProgram`/`resolve_program` 解析与校验，
  含 user/group/umask）、`src/program.rs`（`spawn` 降权、换组、umask 与环境注入）、
  `src/supervisor.rs`（daemon 侧可行性预检、启动加载剔除、compose 预览警告）、
  `src/server.rs`（预览 JSON 增加 warnings）、`src/main.rs`（`ak validate`/`compose`
  输出警告）。
- **测试**：`src/config.rs` 单元测试（用户存在性、数字 uid、Windows 拒绝）、
  `src/program.rs` 或集成测试（root 下降权与 groups，非 root 环境下跳过）、
  xtask e2e 的 compose 场景（非 root 指定他人用户 -> 400 且无副作用）。
- **不进二进制体积**：沿用 `libc`，无新增 crate。
- **不影响**：Windows 进程管理路径、webui 页面、benchmark。
