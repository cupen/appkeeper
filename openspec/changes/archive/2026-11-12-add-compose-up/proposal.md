# Proposal: add-compose-up

## Why

appkeeper 目前没有「声明式编排」入口：`ak run --` 只能单程序临时拉起，`ak compose <部署目录>` 是链接注册（本体留在外部、改完还要手动 apply）。缺一条 docker compose 式的路：项目根放一份声明文件，一条命令把整组程序交给 daemon 启动并注册，声明与已注册内容漂移时由人确认收敛。同时 run 与 compose 的注册产物同居 `app_dir` 顶层、仅靠文件形态（symlink 探测）区分，覆盖权限判据脆弱，运维也无法从落位一眼分辨「临时拉起的」与「声明部署的」。

## What Changes

- **新增 `.appkeeper.toml` 声明文件与 `ak compose up [dir]` 子命令**：
  - 文件格式**零新语法**：完整复用既有 app 配置 schema（`[app]` 可选 + `[program.*]` ≥1、未知字段拒绝；`command` 沿用既有单行带参写法，`args` 可省）；渲染时相对路径以文件所在目录为基准重写为绝对路径、裸命令名按 PATH 解析。
  - 发现规则：缺省在 `dir`（默认 `.`）找 `.appkeeper.toml`，`-f/--file` 显式指定任意文件名；不向上递归。app 名缺省取文件所在目录名，`--name` 覆盖；`[app]` 表不引入 name 字段；沿用 `NAME_RULE` 与保留字 `all`。
  - 执行流水线 daemon-first：守护进程按 priority + depends_on 拉起全部程序（不落盘）→ 逐个过启动判定（startsecs/健康）→ **全部成功才落盘注册**（渲染快照拷贝至 `app_dir` 顶层，非链接）；任一失败 → 停掉本轮已拉起的、零残留、非 0 退出。进度复用 run 的 NDJSON 流式反馈，`--json` 机器可读。
  - 拉起遵循守护启动同款规则：`no` 不拉、`unless-stopped` 有用户停止记录不拉并打印提示；`up` 不清除用户停止记录。
  - 守护离线：隐式后台拉起守护后继续（compose 家族语义），不走 run 的退出码 3。
- **撞名交互语义**（同名已注册时）：
  - 已有 compose 记录且配置不一致 → TTY 交互「强制更新？」：是 = 停旧 → 原子重写 → 按新配置拉起；否 = 零改动、非 0 退出。
  - 配置一致 → 无操作、退出 0；一致但程序没在跑 → 拉起。
  - 已有 **run ad-hoc 记录** → 同一交互，确认后收编（停它 → 删 adhoc 记录 → 落 compose 记录并拉起）。
  - 已有 **symlink 外部部署注册** → 拒绝（不进交互），提示先 `ak remove`。
  - 非 TTY（管道/CI）：需要交互的分支默认拒绝（非 0、零副作用）并提示 `--force`；`compose up --force` 为「是」的直通版。
- **注册表分区**：`app_dir/adhoc/` 子目录承接 run ad-hoc 产物（单程序记录）；compose（脚手架与 `compose up` 渲染快照）与外部链接注册留在顶层。app 名唯一性仍跨全 `app_dir`（含 `adhoc/`）；覆盖权限按**路径**判定（`adhoc/` 内可由 `run --force` 重写；顶层真实文件仅 compose 再生 / `compose up` 可写；symlink 永不写）；`ak ps --all` 按记录位置标注来源（run / compose，纯展示）；既有顶层 run 生成文件不迁移、就地兼容。
- **BREAKING** `run --force` 覆盖范围收窄：现状下 `app_dir` 顶层真实文件（含 compose 脚手架产物）可被 `run --force` 覆盖；分区后顶层一律拒绝（提示 `ak remove` 或改用 `compose up`），只有 `adhoc/` 内的记录可被 `run --force` 重写。
- **声明内 `${NAME}` 占位符与 `-e/--env NAME=VALUE`**：`-e` 逐次传入的值在解析前替换声明文本中的 `${NAME}` 与 `${NAME:-default}`（`$$` 为字面 `$`）；未传值且无默认的名字原样保留给 shell（与 action 变量同一哲学），`${program.x.pid}` 等运行期变量同样保留。值仅在本次调用内生效、绝不落盘（落盘的是替换后的渲染快照）。
- 非目标：`compose down/stop`（已有 `ak stop`/`ak remove`）；单文件多 app；文件监听自动 up；多文件合并；`env_file`/auto-load `.env`/profiles；`up` 的「只注册不跑」形态；`up` 不收内容微调 flag（仅 `-f/--name/--force/--json/-e/--env`）。

## Capabilities

### New Capabilities

（无——全部并入既有能力规范）

### Modified Capabilities

- `app-registry`: 新增「声明式编排（compose up）」要求（文件格式与发现、命名、快照落盘、撞名交互五分支、非 TTY 与 `--force`、执行流水线与离线拉起）；新增「注册表分区」要求（`adhoc/` 落位、全局唯一、路径判覆盖权限、删除语义与 `ps --all` 来源标注、既有文件兼容）；收窄「run ad-hoc --force 强制覆盖」的覆盖范围（顶层 compose 记录不再可被 run 覆盖）。
- `control-plane`: 新增 compose up 的 daemon-first 端点族要求（预览/提交两段协议、NDJSON 进度复用、staged commit/abort 复用、收编与外部注册拒绝的回传形态）。

## Impact

- **代码**：`src/registry.rs`（`adhoc/` 分区、`list` 扫描两处、`remove` 判据扩展、`guard_scaffold_overwrite` 路径化、pending 占位落位）、`src/supervisor.rs`（staged 多程序扩展、渲染比对/收编、启动编排复用）、`src/server.rs`（compose up 预览/提交端点）、`src/client.rs`（预览协议与 NDJSON、TTY 交互）、`src/main.rs`（`compose up` 子命令解析与旗标面）、`src/config.rs`（渲染绝对化与校验复用，无 schema 改动）。
- **测试**：`registry`/`supervisor` 单测更新（分区、收窄后的 run --force、收编）；`xtask` e2e 新增 compose up 段（全新注册、五分支撞名、非 TTY 拒绝、失败 abort 零残留、离线拉起、来源标注、与既有顶层 run 文件共存）。
- **文档**：README 命令速查（`compose up`、`.appkeeper.toml`、`app_dir/adhoc/` 分区、run --force 收窄的迁移提示）与「开发场景速览」（`compose up` 即 dev 一条命令：离线自动拉起、`unless-stopped`/`ak stop` 防复活、Windows 前台形态提示、webui 一次性配置）——原独立 dev 会话方案已合并入本变更（规划工件已删，探索结论沉淀于本条与任务 5.2）。
- **不影响**：app 配置 schema 与四层默认级联、apply/pending 语义、webui、控制面既有端点与准入、bench。
