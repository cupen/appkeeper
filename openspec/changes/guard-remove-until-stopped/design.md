# Design: guard-remove-until-stopped

## Context

`src/main.rs` 当前由 CLI 直接调用 `registry::remove` 删除注册，再调用 `sync_if_online`；`apply_detection` 对检出消失的 app/program 一律 `stop()` 后移除。控制面只有 app 的 start/stop/restart 扇出，Web 控制台也只有程序控制与 apply，尚无移除入口。

Supervisor 命令队列是程序状态的唯一写者；`registry` 负责注册记录与配置路径；控制面 `/v1` 与 Web `/api` 共用 Supervisor。进行中的 `refine-cli-verbs` 正修改同一批 CLI 规范与 app-registry 的「离线可用」需求，本设计不改动该 change，最终合入需保留其 `ak config --reload` 文案。

## Goals / Non-Goals

**Goals:**

- 将“允许移除”的判定和注册删除集中在 Supervisor 命令中，让 CLI、shell、控制面及 Web 复用同一结果。
- 使命令式移除立即收敛内存投影和磁盘注册，不再依赖 reload/apply。
- 对磁盘驱动的 app/program 删除，在 apply 修改任何范围内状态之前完成存活预检。
- 清楚处理自然退出、fatal、停止中、退避等待及落盘失败。

**Non-Goals:**

- 不改变 compose 注册的 pending/apply 流程，也不改变程序配置更新的 apply 重启语义。
- 不增加强制移除或自动 stop+remove 旗标。
- 不删除外部部署配置、日志文件或部署目录内其他内容。
- 不引入新的传输形态、第三方依赖或持久化数据库。

## Decisions

### D1: Supervisor 提供唯一的在线 Remove 命令

新增 `Command::Remove { app, reply }` 与结构化 `RemovedApp` 结果。CLI 不再直接调用 `registry::remove`，而是通过 `client` 请求 `POST /v1/apps/<app>/remove`；Web 提供 `POST /api/apps/<app>/remove`，shell 也复用 `/v1` 客户端操作。两端点均把意图排入同一个 Supervisor 命令队列。

移除在同一条命令处理中依次完成：解析已加载 app 与全部成员 → 检查成员状态 → 读取该 app 的注册记录 → 删除注册记录 → 清理内存中的 app/programs、相关 pending 与用户停止记录 → 同步健康任务及状态发布。磁盘操作期间保留命令处理串行化，防止 start/restart/apply 在检查与删除之间插入。HTTP 线程不自行操作程序或注册表。

**备选：**保留 CLI 直删并在之后 reload。否决：无法保证状态前置检查、所有入口一致性及成功即收敛，也保留二次 apply 的旧问题。

### D2: 活态守卫覆盖运行、启动、停止中与退避

新增统一的 `removable` 判定，供 Remove 与 apply 删除预检共用。`running`、`starting`、`stopping` 表示仍有存活进程或停机尚未收敛；`backoff` 虽暂时无子进程，但等待自动重启，也视为活的生命周期。四者均拒绝移除。`stopped`、`exited`、`fatal` 允许移除，与是否执行过显式 stop 无关。

冲突响应列出阻塞的程序名及状态，并给出先执行 app 级 stop 的提示。未知 app 为 404；活态阻塞为 409；CLI 保持既有错误退出约定，守护不可达为 3。

**备选：**仅按 `child.is_some()` 判定。否决：会允许 backoff 在移除过程中被重启；以“必须显式 stop”判定又会无谓阻止 exited/fatal 的清理。

### D3: 注册删除先行，失败不清理运行状态

在活态检查通过后，使用现有 `registry::remove` 的路径规则删除唯一注册记录：`app_dir` 内真实文件被删除，外部部署配置本体保留。只有删除成功才修改内存状态，因此一般 IO 失败不会改变运行投影，响应为服务端错误而非 409；CLI 不随后调用 sync/apply。

已成功的移除返回应用名、注册记录路径、是否删除生成文件及保留的外部配置路径（适用时）。输出必须让用户理解已立即移除且无需 apply。移除后重新计算/发布 pending，使同一 app 不再遗留可再次应用的注销项，其他 app 的 pending 保留。

**取舍：**不承诺跨文件系统的事务或失败回滚。如果底层删除实际生效但错误仍被返回，不伪造“注册未变”：错误需说明无法确认一致性，要求检查注册路径及 `/v1/status`，而非再次 apply。对通常的删除前失败，状态保持原样；不自动重试删除或停止进程。

### D4: apply 在任何范围内变更之前整批预检删除

`apply_detection` 在现有 detect 后、任何 `programs.remove` / `stop` / 新定义重建之前，比较已加载与检出定义，找出范围内的 app 删除与 program 删除。任何被删除对象处于 D2 的活态时，返回包含全部阻塞对象的冲突错误，且不应用范围内的其他新增、修改或重启。

磁盘已经手工删除的配置不恢复；重新 detect 后删除项仍为 pending。用户 stop 后再次 apply 才执行既有清理。`--restart` 不豁免此前置检查，范围外删除不阻止本次 apply；已被配置隔离而保留旧定义的 app 不视为删除。

**备选：**仅跳过冲突删除并继续其他变更。用户已明确选择整批拒绝，以避免部分生效掩盖危险删除。

### D5: Web 与 shell 只做入口，不复制业务判定

shell 增加 `remove <app>`，帮助与错误提示对应 `ak stop <app>` → `ak remove <app>`。Web App 概况增加移除操作，确认框说明注册及生成配置的删除边界；不先自动调用 stop。成功后通过 REST 响应与 WS/状态刷新清除侧栏和主区 app，若当前选中该 app 则导航至可用概况；409 时保留界面并显示后端给出的阻塞原因。

控制面与 Web API 返回同构结构化结果；WebSocket 沿用既有快照/增量格式，不新增状态字段。`/v1`、`/api`、WS 所需投影变更在同一后端路径产生。

## Risks / Trade-offs

- [命令处理中同步删除文件会短暂阻塞其他控制命令] → 单个注册文件的本地 IO 保持短小；所有状态写仍串行，以一致性换取这点延迟。
- [停止中与退避没有子进程却被拒绝，可能显得严格] → 明确错误列出状态；前者等待停止收敛，后者先 stop 取消自动重启，避免移除后的复活。
- [删除成功与响应丢失之间存在客户端重试歧义] → 未知 app 返回 404，界面不盲目重复提交；结果和状态投影以服务端为准。
- [手工删配置后 apply 从自动清理变为整批拒绝，是破坏性兼容变化] → proposal 标 BREAKING，错误列出阻塞对象与恢复步骤，并增加覆盖范围内其他变更未执行的测试。
- [与 `refine-cli-verbs` 修改同一需求] → 合入时基于其最新 delta 解决文本冲突，不改变其 start/run/reload 语义。

## Migration Plan

1. 先实现共享状态判定、Supervisor Remove 与 `/v1` 端点及后端测试。
2. 切换 CLI 与 shell 至在线端点，移除 CLI 直删和 remove 后 sync。
3. 为 apply 增加整批预检，再增加 Web API/UI；按既有验证矩阵运行测试。
4. 发布说明提示新顺序 `ak stop <app>` → `ak remove <app>`；旧的 remove 后 apply 不再需要。未修改的旧脚本若依赖离线 remove 或 remove 自动停进程，将以明确错误拒绝，不做隐式兼容执行。
5. 回滚代码可恢复旧行为；已成功删除的生成配置不能靠软件回滚恢复，需从原命令重新 compose/run 注册。外部配置未删除，可重新 compose。
