## Why

`ak webui` 的默认模式已经是「探测 → 调系统浏览器 → 打印实际打开的完整 URL」，但这条链路上
有两处把**便利功能**写成了**命令成败**：

1. 浏览器启动失败（没有 `xdg-open`、launcher 返回非 0）被当作命令失败，退出码 1。
2. 可达性探测超时被「按未伺服处理」，退出码 1——而它在打印 URL **之前**，于是最需要地址
   的时刻反而什么都不打印，与本规范自己声明的「无论如何都打印完整 URL」相矛盾。

URL 才是命令的交付物：daemon 已经自述 `enabled && serving`，地址与（auto）token 都已拿到。
主机上没有可用浏览器、探测恰好被代理/防火墙/启动竞态挡住，都不该让用户丢掉地址、也不该让
脚本中断；退出码 1 应当只表示「daemon 自述控制台未启用/未伺服」这类真配置问题。

## What Changes

- **URL 与探测解耦**：daemon 报告 `enabled && serving` 后打印裸 URL，且打印**发生在探测与浏览器
  启动之前**（进程级顺序，不只是数据结构顺序）——探测最多 5 秒、launcher 可能阻塞到浏览器退出，
  晚到的失败只能影响 stderr 与退出码，不能延迟或抑制 URL。成功路径的 stdout 由 `opening <url>`
  改为裸 URL，三种输出形态统一。
- 默认模式的浏览器启动改为 **best-effort**：launcher 缺失或启动失败时，URL 已打印，stderr
  给一行提示，退出码 **0**；launcher 不存在与 launcher 失败走同一条降级路径，不回退其他
  launcher。
- **探测失败也降级**：URL + stderr 一行原因 + 退出码 **0**，不再「按未伺服处理」退 1。
  退出码 1 只保留给 daemon 自述 `enabled == false` / `serving == false` 的配置问题。
- **无图形会话不再探测**：没有浏览器可开，探测没有意义，省掉约 5 秒等待；打印 URL + 提示，
  退出 0。stdout 从此**恰为**裸 URL——无图形会话的提示从 stdout 移入 stderr，旧的两行 stdout
  不再存在（这是可观察的输出变化，不只是「少一次等待」）。
- **降级提示统一为单行 `ak: warning:`**：三种降级各一行，退出码 0，不再用 `ak: error:` 前缀；
  启动失败的旧两行（`error:` + `hint:`）压成一行，把「手动打开 stdout 上的 URL」并入其中。
- `--json` 的 `opened` 定义为「本次调用是否把 URL 交给系统浏览器且启动成功」；输出模式从不
  启动浏览器，故该字段恒为 `false`（原规范列了字段却未定义取值，隐含「可能为 true」）。
- 不变的部分：控制台未启用 / 未成功伺服仍退出 1 且不打印 URL；daemon 离线退出 3 且不打印
  URL；`--print-url` / `--json` 不探测、不打开浏览器、退出 0。
- **BREAKING**（契约放宽）：浏览器启动失败与探测失败的退出码由 1 改为 0；默认模式成功时
  stdout 从 `opening <url>` 变为裸 URL；无图形会话的提示从 stdout 移到 stderr。检查非零退出、
  匹配 `opening ` 前缀或解析无图形会话 stdout 的调用方需要调整（机器可读场景用 `--print-url` /
  `--json`）。
- **能力移除**（明确记录）：改完后没有任何退出码反映控制台可达性——`ak webui` 可以对完全不可达
  的控制台退 0。需要「控制台是否在线」的脚本必须自行请求 `/api/health`，这是把「一条命令」
  换成「手写探测」的净损失；可接受的理由见 design D2。

## Capabilities

### New Capabilities

（无）

### Modified Capabilities

- `control-plane`: `ak webui` 的输出顺序、探测失败语义、浏览器启动失败退出码与 `opened`
  字段语义——URL 先于一切探测交付，便利功能失败不再算命令失败。

## Impact

- `src/webui_open.rs`：拆分纯决策 `plan(status, mode, gui) -> Plan`（`Unavailable` /
  `Print` / `Open`）与副作用 `execute(gui, probe, launch) -> (code, notes)`；探测失败与启动失败
  分支都改为 stderr 一行 `ak: warning:` + `code` 保持 0；成功分支不再追加 `opening` 行。相关
  单元测试重写。
- `src/main.rs`：`webui_cmd` 改为消费 `Plan`（`Open` 先打印 URL 并 flush，再执行副作用）；
  其文档注释描述的旧契约（「failed launch are exit 1」「token never by the regular success
  line」）必须同步更新。
- 交互 shell 的 `open`（`src/shell.rs`）复用 `launch_browser` 与 URL 构造，但输出格式不同
  （仍打 `opening <url>`）：`shell-client` 规范只约束 URL 与提示，没有进程退出码契约，本变更
  不改变它的行为；`open_webui` 里「Same contract as `ak webui`」的注释需改为「共享 URL 构造」。
- 规范：`openspec/specs/control-plane/spec.md` 的 `ak webui` 条款（打印顺序、探测失败、
  启动失败、降级提示前缀、`opened`、场景表）。
- 测试：e2e W6b 只用 `--print-url` / `--json`，断言 `opened == false` 与空 stderr 仍成立；
  新增 e2e 覆盖无图形会话与 launcher 缺失两条降级（只需 env 覆盖），探测失败仍只有单测。
