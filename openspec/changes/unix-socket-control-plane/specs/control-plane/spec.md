## MODIFIED Requirements

### Requirement: 本地 HTTP API

守护进程 SHALL 在**本地 Unix domain socket**（Windows 上为命名管道）上提供 JSON API，socket 上承载的协议仍为 HTTP/1.1：MUST NOT 监听任何控制面 TCP 端口，MUST NOT 绑定任何网络地址。API SHALL 提供以下端点（端点集合、请求与响应结构、状态码语义均不因传输层变更而改变）：

- `GET /v1/health`：守护进程存活探测
- `GET /v1/status`：守护进程信息与全部程序的状态汇总
- `GET /v1/programs`：程序列表
- `GET /v1/programs/{name}`：单个程序详情（所属应用、状态、pid、存活时长、重启/退出计数、健康状态、等待原因、CPU 利用率与驻留内存（可空，metrics 能力口径）、stdout/stderr 日志输出速率）
- `POST /v1/programs/{name}/start` / `stop` / `restart`
- `POST /v1/programs/{name}/signal`：向该程序的子进程投递白名单信号，请求体指定信号名
- `POST /v1/programs/{name}/actions/{action}`：执行该程序声明的自定义动作，同步等待至完成或超时，返回退出码、耗时与截断输出
- `POST /v1/apps/{name}/start` / `stop` / `restart`：app 级扇出动作，按启动排序（停止为逆序）作用于该 app 全部程序，返回逐程序结果
- `GET /v1/programs/{name}/logs`：按 `stream=out|err`、`tail=N` 查询日志
- `POST /v1/reload`：重扫配置并检出待应用变更，返回 pending 预览（不应用）
- `GET /v1/pending`：查询当前待应用变更（逐程序 changed 清单、app 级注册变化、不可热更字段提示、检出错误）
- `POST /v1/apply`：应用待应用变更，请求体可指定范围（app、program）与 `restart` 布尔参数，返回逐程序结构化结果
- `POST /v1/shutdown`：停止全部程序并退出守护进程

程序详情与状态汇总中的指标字段为增量扩展：仅新增可空字段，既有字段名与
语义不变，既有消费者（含 CLI 控制命令）行为不受影响。

#### Scenario: 状态汇总可查询

- **WHEN** 向运行中的守护进程请求 `GET /v1/status`
- **THEN** 返回 JSON，包含守护进程信息与每个程序的名称、状态、pid 与 unhealthy 标记

#### Scenario: reload 返回 pending 预览

- **WHEN** app 配置修改后向运行中的守护进程请求 `POST /v1/reload`
- **THEN** 返回 pending 预览 JSON（含变更程序清单），无任何程序被停止或重启

#### Scenario: pending 可独立查询

- **WHEN** 配置存在未应用的变更时请求 `GET /v1/pending`
- **THEN** 返回逐程序 changed 清单与 app 级注册变化；无变更时返回空清单

#### Scenario: apply 带范围与 restart 参数

- **WHEN** 请求 `POST /v1/apply`（body 含 `{"app": "A", "restart": false}`）
- **THEN** 仅应用 app A 的 pending，返回逐程序 changed/action/result 的 JSON

#### Scenario: 非法状态转换被拒绝

- **WHEN** 对已经 `running` 的程序发送 `POST /v1/programs/{name}/start`
- **THEN** 返回 409 与解释性错误，程序状态不受影响

#### Scenario: 未知程序返回 404

- **WHEN** 请求不存在的程序名
- **THEN** 返回 404 与错误信息

#### Scenario: shutdown 优雅退出

- **WHEN** 发送 `POST /v1/shutdown`
- **THEN** 守护进程按关闭流程停止全部程序后进程退出

#### Scenario: 详情含指标且向后兼容

- **WHEN** 请求 `GET /v1/programs/{name}` 且该程序运行中
- **THEN** 详情含可空的 cpu_percent、mem_bytes 与日志速率字段；不带新
  字段消费旧 JSON 的既有客户端仍能解析全部既有字段

#### Scenario: app 扇出动作

- **WHEN** 请求 `POST /v1/apps/gateway/start`
- **THEN** gateway 的全部程序按排序规则启动，返回逐程序结果；未知 app 返回 404

#### Scenario: 执行自定义动作

- **WHEN** 请求 `POST /v1/programs/api/actions/flush`
- **THEN** 动作同步执行，返回含退出码、耗时、是否超时与输出尾部的 JSON；未知动作名返回 404

#### Scenario: 动作执行中冲突

- **WHEN** 同一程序的同名动作执行中再次请求
- **THEN** 返回 409 与冲突说明，原执行不受影响

#### Scenario: signal 投递

- **WHEN** 请求 `POST /v1/programs/web/signal`（body `{"signal": "USR1"}`）
- **THEN** 信号投递给该程序子进程并返回成功；非白名单信号返回 400 与白名单说明

#### Scenario: 守护进程不监听任何控制面 TCP 端口

- **WHEN** 守护进程启动完成
- **THEN** 进程不监听任何控制面 TCP 端口；运维可直接用
  `curl --unix-socket <socket 路径> http://localhost/v1/status` 查询状态

## REMOVED Requirements

### Requirement: API 鉴权

**Reason**: 「未配置即无鉴权、仅依赖回环绑定」的安全模型被本地 socket 端点的属主判定取代。控制面不再监听网络端口，`auth_token` 作为应用层凭据失去存在基础——决定谁能控制守护进程的问题已由操作系统在 `connect()` 阶段强制回答，继续保留一个默认关闭、需用户主动配置的 token 反而制造了「配了才安全」的错觉。

**Migration**: 删除 daemon 配置中的 `auth_token` 键（否则校验失败、拒绝启动）。依赖 token 的自动化脚本无需改动：控制面不再要求凭据，原先携带 `Authorization` 头的调用方 SHALL 继续正常工作，服务端忽略该头。Web 控制台的访问控制不受影响，由新增的 `[webui] auth_token` 独立承担（见 `webui-api` 的「控制台访问控制」）。

### Requirement: 启动冲突处理

**Reason**: 该 requirement 的判定依据是「控制端口是否被占用」，而控制面不再有端口。冲突判定的对象变为 socket 端点，且新增了旧语义不存在的分支——崩溃残留的陈旧端点需要与真实占用区分开，否则守护进程会因上次异常退出而永远无法启动。

**Migration**: 无需用户操作。行为由「启动冲突与陈旧端点处理」完整承接，仅判定对象从端口改为本地 IPC 端点。

## ADDED Requirements

### Requirement: API 准入控制

控制面 SHALL NOT 使用应用层凭据鉴权。请求的准入 SHALL 完全由操作系统依据本地 IPC 端点的属主判定：unix 上控制 socket 的权限位 SHALL 为 `0600`、其所在 uid 目录 SHALL 为 `0700`，Windows 上控制管道的 DACL SHALL 仅允许守护进程运行账户、SYSTEM 与 Administrators，并显式移除 Everyone / Users / Authenticated Users。

`[daemon] auth_token` 键 SHALL 被移除，MUST NOT 保留旧键的兼容读取路径或回退逻辑。除 `/v1/health` 外的端点同样受此约束：SHALL NOT 存在任何绕过端点权限的豁免。非属主用户 SHALL 在 `connect()` 阶段即被内核拒绝，SHALL NOT 收到任何 API 响应（含 401 或 404），程序状态与日志 MUST NOT 被泄露。服务端 SHALL 忽略请求携带的任何 `Authorization` 头，MUST NOT 因其存在或缺失改变响应。

#### Scenario: 非属主用户无法连接

- **WHEN** 以非守护进程运行账户的用户尝试连接控制 socket
- **THEN** 连接在内核层被拒绝（权限错误），无任何 API 响应返回，程序状态不被泄露

#### Scenario: 属主用户正常连接

- **WHEN** 以守护进程运行账户的用户请求 `/v1/status`
- **THEN** 正常返回 200 响应；响应中不包含任何鉴权相关字段

#### Scenario: 携带旧 token 头不受影响

- **WHEN** 请求不带任何 `Authorization` 头的 `/v1/status`，且调用者为 socket 属主
- **THEN** 返回 200；服务端不读取、不校验、不记录任何 token

### Requirement: 控制 socket 端点

守护进程 SHALL 通过一个可配置的本地 IPC 端点对外服务，默认路径为 `/run/appkeeper/<uid>/ak.sock`（unix）与 `\\.\pipe\<用户名>.sock`（Windows），两者均把当前 uid / 用户名纳入路径以支持同机多用户并存。unix 上 `/run/appkeeper` 父目录 SHALL 为 `0755`、其下 uid 子目录 SHALL 为 `0700`，守护进程 SHALL 在缺失时创建这些目录。配置 `[daemon] socket` 时 SHALL 以该值覆盖默认路径，该值 SHALL 为非空字符串；解析失败 SHALL 被校验拒绝。

客户端 SHALL 从同一配置解析端点路径。`ak shell`、`ak run` 及全部控制子命令、后台拉起协议的就绪探测、基准测量工具 SHALL 全部经同一端点通信，MUST NOT 存在绕过该端点的第二条控制面路径。

#### Scenario: 默认路径含 uid 分层

- **WHEN** 未配置 `[daemon] socket`，守护进程以 uid 1000 启动
- **THEN** 控制端点为 `/run/appkeeper/1000/ak.sock`，其所在目录权限为 `0700`

#### Scenario: 同机多用户并存

- **WHEN** root 用户与 uid 1000 用户各自运行一个守护进程实例
- **THEN** 两者使用各自的端点，互不冲突；任一方的客户端无法连接另一方的端点

#### Scenario: 自定义 socket 路径

- **WHEN** 配置 `[daemon] socket = "/tmp/ak-test.sock"` 后启动守护进程
- **THEN** 控制端点创建于该路径，客户端从同一配置解析并成功通信

#### Scenario: 非法 socket 值被拒绝

- **WHEN** 配置 `[daemon] socket = ""`（空值）
- **THEN** 校验失败并拒绝以该配置启动

### Requirement: 启动冲突与陈旧端点处理

守护进程启动时若控制端点路径已被占用，SHALL 先尝试连接该端点以判定占用性质：连接成功说明已有实例在运行，SHALL 以明确的错误信息退出（非零码）并提示可能已有实例在运行；连接被拒（无监听者）说明是上次异常退出留下的陈旧端点，SHALL 删除该端点后重建并正常启动。删除前 SHALL 校验该路径确为 socket/管道端点而非任意文件，MUST NOT 删除非端点路径。同机重复启动 SHALL NOT 静默成功。

#### Scenario: 已有实例占用时拒绝启动

- **WHEN** 第二个守护进程实例以相同配置启动，且能连接到既有端点
- **THEN** 启动失败，错误信息指出已有实例正在运行，且既有实例的端点未被删除

#### Scenario: 陈旧端点被清理后正常启动

- **WHEN** 控制 socket 路径存在但连接被拒（上次进程崩溃残留）
- **THEN** 守护进程删除该陈旧端点并重新绑定，正常启动；既有程序不受影响

#### Scenario: 非端点路径不被删除

- **WHEN** 目标路径存在但不是 socket/管道端点
- **THEN** 启动失败并明确报告该路径被非端点文件占用，MUST NOT 删除该文件

### Requirement: 控制面连接失败的错误区分

客户端连接控制端点失败时 SHALL 区分两类原因并给出不同提示：**端点不存在**（守护进程未运行）以退出码 3 报「守护进程不可达」，与既有退出码约定一致；**端点存在但当前用户无权连接**（权限错误）SHALL 以退出码 3 退出，错误信息 SHALL 指明端点属主与当前用户不一致并提示需要更高权限，而不是笼统的「不可达」。

#### Scenario: 守护进程未运行时不可达

- **WHEN** 守护进程未启动时执行 `ak status`
- **THEN** 输出守护进程不可达的提示，退出码为 3

#### Scenario: 权限不足的专属提示

- **WHEN** 守护进程以 uid 0 运行，而当前用户为 uid 1000，执行 `ak status`
- **THEN** 退出码为 3，错误信息指出控制端点属主为 uid 0、当前用户为 uid 1000，并提示需要更高权限；MUST NOT 表述为守护进程未运行
