## Context

- 子进程只有一个 spawn 点：`src/program.rs:212` 的 `Command::new`，所有拉起路径（守护启动、
  reload、apply、compose/run 的 staged 提交）最终都经过它。
- 配置侧是 `ProgramRaw`（`src/config.rs:1281`，`deny_unknown_fields`）→ `resolve_program`
  → `ResolvedProgram`；`def_hash` 由 `ResolvedProgram` 的 serde JSON 计算，新字段会自动
  进入变更检测。
- compose 声明的渲染/校验发生在 **daemon 侧**（`cmd_compose_up` → `resolve_app` →
  `validate_all`，失败即 400 且零副作用）；而 `ak validate` 在 **客户端**直接调
  `resolve_app`（`src/main.rs:2718`）。
- 守护进程身份由启动方式决定（前台、后台、systemd/Windows 服务）。系统服务通常以 root
  运行，用户自启的 daemon 通常是普通用户。

## Goals / Non-Goals

**Goals:**

- 程序级运行身份声明，缺省继承 daemon 身份，配置保持机器无关。
- 身份问题全部在「写盘/启动之前」暴露：未知账号、非 root 跨用户、Windows 误用。
- 降权正确：附加组、HOME 等身份环境变量、进程组语义都不被破坏。

**Non-Goals:**

- 不做 `ak run` 的 ad-hoc `--user`、不在 `ak ps`/状态投影里新增 user 列。
- 不让 health_check、自定义 action 以目标用户执行（它们仍以 daemon 身份运行）。
- 不做 capability/容器场景的细粒度特权（只按 euid==0 判定可行性）。
- 不写任何用户态/启动失败后的自动提升或回退。

## Decisions

### D1 字段落在 `[program.<name>]`

`ProgramRaw` 增加 `pub user: Option<String>`，`ResolvedProgram` 增加
`pub user: Option<UserSpec>`（`Serialize`，自动进 `def_hash`）。

- 备选：`[app].user` 默认 + 程序覆盖（走 `LevelOptions` 三层合并）。否决理由：本次只有
  一个消费者，程序级已覆盖全部场景；引入 app/daemon 两层会扩大 spec 面与合并逻辑。
- compose 快照 `render_declaration_with_vars` 对程序字段是原样透传
  （`raw.program.get(&rp.name).cloned()`），声明里的 `user` 不需要额外搬运代码。

### D2 解析期把名字解析成身份描述，但**不**做特权判断

`resolve_program` 产出三个字段：`user: Option<UserSpec>`、`group: Option<GroupSpec>`、
`umask: Option<u32>`。

- `UserSpec { name, uid, gid, groups: Vec<gid_t>, home, shell }`：名字优先 `getpwnam_r`；
  纯数字先按名字查找，失败再 `getpwuid_r`；账号不存在 → `Err`（`ak validate`、compose
  预览、daemon 启动加载都能早发现）；附加组用 `getgrouplist` 预取，供 `pre_exec` 使用
  （`initgroups` 会读 /etc/group、分配内存，不是 async-signal-safe）。
- `GroupSpec { name, gid }`：名字走 `getgrnam_r`，数字走 `getgrgid_r`；都要求组存在
  （与 `user` 的数字形式保持一致的「拼写早发现」策略）。
- `umask: Option<u32>`：字符串按八进制解析（允许 `0`/`0o` 前缀，1–4 位，上限 `0o777`），
  非法值在解析期报错。

**有效身份**：运行 uid = `user.uid` 或 daemon euid；有效主组 = `group.gid`，否则
`user.gid`，两者都没有则 daemon egid；附加组 = `user.groups`，`group.gid` 不在其中时并入。

特权可行性判断放在 daemon 侧 `check_user_feasible`，不放进 `resolve_app`。理由：`ak
validate` 以调用者身份运行，若在其中比较 euid 与目标 uid，普通用户校验 root daemon 的
配置会误报。备选「spawn 时才失败」被否决：会掉进重启退避循环，违背「执行前拦截」。

### D3 可行性规则与调用点

```
euid == run uid 且 eff gid == egid          -> 可行，零切换调用
euid == 0                                   -> 可行，按需 setgroups/setgid/setuid
euid == run uid 且 eff gid != egid          -> 仅当属于该组（getgroups）才可行，只 setgid
其他                                        -> 不可行
run uid == 0                                -> 可行但告警
```

「属于该组」在非 root 场景下是硬约束：`setgid` 只允许切到真实/保存 gid 或附加组之一，
daemon 不是成员时不可能完成，必须在提交前拒绝。调用点：

- `cmd_compose_up` / `cmd_adhoc_up`：staging 之前 → `ComposeUpError::Validation`（400，
  零副作用）。
- daemon 启动加载与重扫：不可行 app 剔除并 `warn!`（沿用 `ListedApp` 坏配置的隔离语义），
  不因单 app 配置拖垮整个 daemon。
- `ak validate`：只对「非 root 且目标不同」打印 warning，不判失败（它无法知道 daemon
  的实际身份）。

### D4 降权实现：`pre_exec` 里的 `setgroups -> setgid -> setuid -> umask`

- 不用 `std::os::unix::process::CommandExt::uid/gid`：它只调 `setuid/setgid`，会保留
  root 的附加组，也无法表达「只换组」。
- `cmd.process_group(0)` 先注册，再注册 `pre_exec` 闭包；闭包 move 进已解析好的
  `Vec<gid_t>`、uid、有效 gid 与 umask。闭包内只调 AS-safe 的系统调用，失败返回
  `io::Error::last_os_error()`，顺序固定：
  1. root 路径：`setgroups(附加组 ∪ 有效主组)` → `setgid(有效主组)` → `setuid(运行 uid)`
  2. 非 root 同用户换组：只 `setgid(有效主组)`（MUST NOT 调 `setgroups`，无权限）
  3. 声明了 `umask`：`umask(值)`（与上述顺序无关，放在最后便于阅读）
- 有效身份与 daemon 完全一致时完全跳过（普通用户跑自己名下的程序没有任何额外系统调用）。
- `umask(2)` 只影响子进程自己创建的文件；daemon 的 pump 线程写日志不受影响（不同进程）。

### D5 身份环境变量只在未显式设置时注入

`def.env` 是 `BTreeMap`，spawn 时逐一检查 `HOME`/`USER`/`LOGNAME`/`SHELL`：`def.env`
有值则保留，否则用 `UserSpec` 的 passwd 值。备选「先 envs 再无条件覆盖」被否决：会
悄悄吞掉用户显式配置。

### D6 告警走两条通道，但不改变退出码

- daemon 侧：app 加载时对每个 root 身份程序 `warn!` 一次（不在每次 spawn 重复刷日志）。
- CLI 侧：`ComposePreview` 增加 `warnings: Vec<String>`，`/v1/compose/up/preview` 回传；
  `ak compose up` 打印到 stderr。`ak validate` 同样打印本地警告。
- 备选「只写 daemon 日志」被否决：`ak compose up` 用户看不到，等于没有提示。

### D7 Windows 明确拒绝

`ProgramRaw` 跨平台保留该字段（结构体统一），`#[cfg(windows)]` 下解析到非空值即
`bail!("user is only supported on unix ...")`。备选「Windows 静默忽略」被否决：用户会以为
身份生效了。

### D8 数字 uid

`user = "1000"` 接受，经 `getpwuid` 解析；查不到的同名/数字值一律报错——因为 HOME、shell、
附加组都依赖 passwd 记录，允许「裸数字无记录」会让环境注入出现空洞。

### D9 失败语义

- `setuid` 等调用失败属于 spawn error，沿用既有重启/退避逻辑。
- 防御性重复：正常路径下 D3 已拦截，spawn 期失败只可能来自容器缺少 CAP_SETGID/CAP_SETUID
  等环境差异，日志需带 errno。

### D10 `group` 是独立字段，而不是从 `user` 推导

`user` 与 `group` 分开：`group` 缺省取运行 uid 的主组（保持「只写 user 就够用」），
显式值时覆盖主组，并且允许不写 `user` 只写 `group`。备选「只提供 `user`，组永远跟随
passwd」被否决：user=deploy、group=www-data 是常见部署形态，且只有 group 的场景
（同用户换主组）在非 root 下也能成立。

### D11 `umask` 只接受字符串形式的八进制

字段类型为 `Option<String>`，解析期转 `u32`。备选「TOML 整数」被否决：`umask = 27`
会被读成十进制 27 而不是八进制 0o27，`umask = 0027` 在 TOML 里直接非法；要求引号写法
（`"0027"`）把歧义消灭在配置层。传整数 SHALL 报错并提示改用带引号的八进制字符串。

## Risks / Trade-offs

- [远程/异构校验] `ak validate` 在本机解析账号；校验一台远端 daemon 的配置时，账号可能
  不存在于本机 → 会误报。缓解：`ak validate` 文档说明按本机解析；daemon 侧才是权威。
- [work_dir 不可访问] root daemon 降权后目标用户可能读不到 `work_dir` → 运行期启动失败。
  缓解：不做 root 下的 `access(2)` 预判（root 视角恒真），错误信息保留命令与 errno。
- [health_check / action 身份不一致] 健康探测与自定义动作仍以 daemon 身份执行，可能访问
  不到目标用户的资源（如 0600 unix socket）。缓解：文档写明；这是既有限制，不在本次扩大。
- [日志属主] 日志由 daemon 的 pump 线程写入，属主是 daemon 用户而非目标用户。与现有行为
  一致，不改。
- [容器/Capability] 只用 `euid == 0` 判定，带 CAP_SETUID 的非 root 容器会被判不可行。
  缓解：保守方向（拒绝而非错误启动），后续可按需放宽。
- [root 告警噪音] 每 app 加载一次；reload 会重新加载 → 告警按次出现，可接受。
- [组库缺失] `group` 要求组条目存在；容器里常见的「裸 gid、无 /etc/group 条目」会被
  拒绝。缓解：错误信息带上解析到的 gid 与配置写法；后续可放宽为数字 gid 不查库。
- [非 root 换组的判定边界] 只按 `getgroups` 判定成员资格；通过 ACL 等外部机制授权的组
  仍会被判不可行。缓解：保守拒绝并在提示中建议使用 root daemon 或保持同组。
- [umask 书写错误] 用户可能写成整数 `umask = 27`。缓解：解析失败的错误信息直接给出
  正确示例 `umask = "0027"`。

## Migration Plan

- 纯新增可选字段：无迁移、无破坏性变更；旧配置解析结果不变（`user: None`）。
- 回滚：删除字段即可；已写入配置文件里的 `user` 在旧版本会因 `deny_unknown_fields` 被
  拒绝，因此回滚前需清掉该字段（发布说明注明）。

## Open Questions

（无 —— 剩余的 `ak ps` 展示、`run --user` 已明确列为非目标。）
