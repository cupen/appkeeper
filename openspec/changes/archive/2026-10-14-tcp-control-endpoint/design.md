## Context

见 proposal.md 的 Why。塑造实现的现状事实：

- 传输层抽象（`trait Conn: Read + Write`、`Endpoint::parse` / `connect` / `bind`、`BindError` 三分支）已就位，且 `handle_connection` 对传输透明——HTTP/1.1 字节在 UDS 与 TCP 上完全相同，本变更因此只需要端点解析与监听器各加一个分支。
- 服务端每连接一线程的模型不变；`handle_connection` 此前删除了 token 参数，本变更以「端点形态」为粒度恢复：形态是 daemon 启动时确定的单一事实，不是 per-request 的双轨。
- `auth_token` 已在 `REMOVED_DAEMON_KEYS` 死键清单里（加载、validate、config set/get/delete 四条路径引用），本变更把它改成条件键——配置面唯一一处从「静态白名单」到「组合校验」的扩权。
- 主规范 `control-plane` 的「API 准入控制」（上一次 change 刚写入）断言「SHALL NOT 使用应用层凭据」且「忽略任何 Authorization 头」——本变更将其从无条件断言收窄为路径形态的断言，`tcp://` 形态走独立的准入条款。这不是回到旧世界：旧世界是「默认 TCP、token 可选、忘了配就敞开」；新形态是「默认 IPC、TCP 显式选择、不配 token 启动都过不了」。
- 浏览器与 webui 不受影响（`[webui] listen` 独立、凭据门独立）。

## Goals / Non-Goals

**Goals:**

- 两种形态在协议与客户端代码上是同一个东西（用户观点：仅 URL 不同）——拨号、重试、错误分类、退出码全部复用。
- 形态与准入机制一一对应，无混合态：路径形态 = 内核属主；`tcp://` 形态 = Bearer token。不存在「TCP 无 token」的可配置状态。
- 默认配置（只写 `socket = "<路径>"` 或什么都不写）的安全属性与现状逐字相同。
- 远程管理零新增机制：一份指向 `tcp://` 的配置就是全部所需。

**Non-Goals:**

- 不做同时双监听（同一 daemon 同一时刻只服务一种形态）。
- 不做 TLS——`tcp://` 形态定位为受信网络或 ssh 隧道内的明文 HTTP，与项目「本地控制面无需 TLS」的既有取舍一致（README 说明此边界）。
- 不恢复 `host` / `port` 两个死键（`tcp://host:port` 写在一个值里）。
- 不改 webui、健康探测、bench 拓扑逻辑。

## Decisions

### D1 单键双形态，显式 scheme 区分

`socket` 值以 `tcp://` 前缀区分形态：有前缀 → TCP（host:port 必须可解析），无前缀 → 本地 IPC 路径。缺省值不变（unix `/run/appkeeper/<uid>/ak.sock`、Windows 管道名）。

- 否决「独立 `listen` + `listen_token` 键」：两个键表达一个事实，还得回答「两个都配了算什么」；单键把「二选一」写进类型。
- 否决「裸 `host:port` 自动识别为 TCP」：路径与 `host:port` 在字符串上可能歧义（`./7310`），显式 scheme 消除猜测；错误信息也因此可以精确。
- 否决「同时监听两种形态」：多出「客户端默认拨哪个」「两形态准入不一致怎么办」两类问题，而真实场景（本地 CLI + 容器健康检查）可以由「二选一 + ssh 隧道/反向代理」覆盖；若未来确有需求，叠加一个 change 的成本不变。

### D2 准入机制跟形态走，token 条件必配

`auth_token` 合法性由 `socket` 形态决定：`tcp://` → 必配非空；路径 → MUST NOT 出现。validate 与加载期同一套组合校验，错误信息给出修复方式（改形态或补 token）。

- 否决「token 可选 + 启动时警告」：忘了配 = 任何本地用户可 stop/shutdown，警告挡不住；把不安全状态变成不可启动状态，是本 change 的安全底线。
- 否决「恢复无条件 `auth_token`、路径形态也允许 token」：路径形态的 token 是死重（内核已判定属主），且让「形态 ↔ 准入」映射出现例外。

### D3 TCP 形态的鉴权语义沿用旧 Bearer 行为

除 `/v1/health` 外全部 `/v1/*` 要求 `Authorization: Bearer <token>`，缺失/不匹配 401；`/v1/health` 免凭据（就绪探测、编排健康检查、bench 连通性探测依赖它）。客户端在 TCP 形态自动附带该头；路径形态不附带且服务端忽略之。token 比较使用非常量时间比较（沿用上次删除前的实现惯例即可，敏感度与 webui token 一致）。

- `/v1/health` 免凭据的边界：它只回答存活，不泄露任何程序状态（规范既有条款），与编排健康检查的兼容性由此保住。

### D4 客户端与错误语义完全形态无关

`Client` 从配置解析端点后不区分形态拨号；TCP 形态 connect 失败的分类沿用 `connect_error` 的既有两分支（不可达 / 权限不足），TCP 只有不可达与 401 两类新错误：401 作为业务答案透传（与 409/404 同类），提示中说明「tcp:// 形态需要 auth_token」。退出码 0/1/2/3 不变。

### D5 bench 与 e2e 的形态覆盖

`--socket` 接受双形态（同一 `Endpoint::parse`），`--token` 恢复且仅 tcp:// 有意义（路径形态下提供 `--token` 属用户错误，报错说明该形态无凭据）。e2e 增加 TCP 形态全链路用例（起 daemon 于随机空闲端口的 `tcp://` + token，全部控制子命令过一遍，含 401 路径），确认「两种形态同一套验收」。

## Risks / Trade-offs

- **TCP 形态被误用于不受信网络** → README 明写「明文 HTTP，token 是唯一防线，跨公网必须自带 TLS 隧道」；不做 TLS 是刻意取舍。
- **`auth_token` 从死键变条件键，配置工具的报错分支变多** → 组合校验集中在 config.rs 一处，四条引用路径（load/validate/set/get）共用同一函数；单测逐分支覆盖。
- **`/v1/health` 免凭据被滥用** → 它的响应体不含任何程序信息（仅 `{"status":"ok"}`），信息泄露面为零；规范条款明确其豁免边界。
- **形态字段名 `socket` 承载 TCP 语义引起误解** → 文档与模板注释明确「socket = 控制端点 URL，形态由 scheme 决定」；改名（如 `endpoint`）是更大的破坏面，收益不足，放弃。

## Migration Plan

1. transport 的 TCP 实现与 Endpoint 解析先行（无行为变化）。
2. 服务端按形态选监听器 + token 门；客户端拨号与 401 透传。
3. 配置组合校验 + `auth_token` 出死键清单；模板注释回填。
4. bench `--token`、e2e TCP 用例、README。
5. 升级零动作：默认形态不变；死键报错行为对 `host`/`port` 保持不变。

回滚策略：单提交可回滚；TCP 形态是纯增量选项，回滚后已有 `tcp://` 配置会因解析失败被拒绝启动（错误信息明确），不存在半态。

## Open Questions

（无——形态互斥、token 条件必配、单键双形态均已在对话中拍板。）
