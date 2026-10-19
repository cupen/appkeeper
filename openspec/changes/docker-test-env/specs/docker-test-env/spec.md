## Purpose

定义 appkeeper 的 Docker 化测试环境能力：以 `debian:bookworm-slim` 为基座的一次性全量验收器——一条 `docker run` 自动跑完单元测试、webui 三连与 e2e（含浏览器段），退出码如实反映成败，报告落回宿主机，且依赖缓存预烘后支持离线复跑。

## ADDED Requirements

### Requirement: 测试镜像构建与依赖完备性

仓库 SHALL 提供一个以 `debian:bookworm-slim` 为基座的测试镜像构建配方（Dockerfile），`docker build` 一次即产出可运行全量验收的镜像。镜像 SHALL 预装全量验收所需的完整工具链与外部命令：Rust（版本可用 build-arg 覆盖，缺省 stable）、Node（缺省 22）、pnpm（缺省与 `webui/package.json` 的 `packageManager` 字段一致）、playwright 与 chromium 浏览器（含其运行所需系统库）、`curl`、`ca-certificates`、C 链接工具链（gcc 与 libc 头文件）。工具链版本 MUST NOT 取自 bookworm 发行版仓库（rustc 1.63 / nodejs 18 均低于本项目 edition 2024 与 Node ≥ 22 的下限）。e2e 其余外部命令依赖（`sh`、`sleep`、`id`、`script`）由基座镜像自带，MUST NOT 重复安装。

镜像 MUST NOT 包含项目源码：构建上下文仅引用锁文件与 manifest（`Cargo.lock`、各 `Cargo.toml`、`webui/pnpm-lock.yaml`、`webui/package.json`）用于依赖预烘，从而使同一镜像跨 commit 复用，仅在锁文件变更后需重建镜像刷新缓存。

#### Scenario: 缺省参数构建即用

- **WHEN** 在仓库根执行测试镜像的缺省 `docker build`
- **THEN** 构建成功，产出的镜像可直接以挂载仓库的方式运行全量验收，无需任何额外安装步骤

#### Scenario: 镜像不含源码仍可跨 commit 复用

- **WHEN** 用 commit A 的锁文件构建镜像后，挂载 commit B（源码变更、锁文件未变）的仓库运行验收
- **THEN** 验收正常执行，不需要重建镜像

### Requirement: REGION 线路开关

镜像构建 SHALL 提供单一 `REGION` build-arg 作为下载线路总开关：缺省 `global`，全部依赖（rustup/crates.io、Node、npm registry、playwright 浏览器 CDN）走官方源；传 `cn` 时统一切换大陆镜像（rustup 与 crates 走 rsproxy、npm 与 playwright 走 npmmirror 系镜像、Node 改走二进制镜像而非 NodeSource apt 源）。两线路 MUST 产出功能等价的镜像；不认识的 REGION 值 MUST 使构建失败并说明可选值，MUST NOT 静默回退缺省线路。

#### Scenario: 缺省 global 线路

- **WHEN** 不传 `REGION` 执行构建
- **THEN** 构建过程全部从官方源下载，产物镜像满足依赖完备性要求

#### Scenario: cn 线路构建

- **WHEN** 执行 `docker build --build-arg REGION=cn`
- **THEN** 构建过程改从 rsproxy/npmmirror 系镜像下载，构建成功且产物镜像满足依赖完备性要求

#### Scenario: 未知 REGION 拒绝

- **WHEN** 执行 `docker build --build-arg REGION=eu`
- **THEN** 构建以非零码失败，错误信息列出 `global`/`cn` 为可选值

### Requirement: 依赖缓存预烘与离线复跑

镜像构建期 SHALL 完成依赖预烘：按当前 `Cargo.lock` 将全部 crates fetch 进镜像内 CARGO_HOME、按 `webui/pnpm-lock.yaml` 将包 fetch 进 pnpm store、chromium 落浏览器缓存目录。验收运行在锁文件与镜像预烘一致时 SHALL 完全离线可用（`--network none` 亦可通过全量验收）；锁文件漂移时 SHALL 自动回退联网拉取新依赖而非直接失败。playwright 的预装位置 MUST 能被 `xtask e2e` 的解析逻辑发现（npx 缓存目录或 `webui/node_modules/playwright`），浏览器段不得因解析失败而静默降级。

#### Scenario: 离线全量验收

- **WHEN** 锁文件未变，以 `docker run --network none` 挂载仓库运行验收
- **THEN** 三段套件全部正常执行并通过，无任何联网请求

#### Scenario: 锁文件漂移回退联网

- **WHEN** 挂载仓库的 `Cargo.lock` 比镜像预烘的更新（新增依赖），以常规网络运行验收
- **THEN** 镜像缓存命中旧依赖、联网仅拉取缺失部分，验收正常完成

### Requirement: 一次性全量验收执行

测试镜像 SHALL 以一次性验收器为运行契约：`docker run` 挂载仓库（约定路径 `/work`）后自动依次执行三段套件——`cargo test --workspace`、webui 三连（`tsc --noEmit`、`vitest`、`vite build`）、`cargo run -p xtask -- e2e`（含 playwright 浏览器段与 bench firehose 冒烟），全程无人值守。任一段失败 SHALL 立即停止后续阶段并以非零退出码结束；全部通过退出码为 0。各阶段日志与 bench JSON 报告 SHALL 统一写入约定输出目录（`/out`），由运行方挂载回宿主机。镜像 SHALL 同时保留通用容器入口（如 `docker run -it <镜像> bash`）供人工检查，但一次性验收 MUST NOT 依赖任何交互输入。

#### Scenario: 全量通过

- **WHEN** 挂载一份可全绿的工作区执行验收容器
- **THEN** 三段套件按序全部执行，容器退出码为 0，输出目录含各阶段日志与 bench JSON 报告

#### Scenario: 阶段失败即停并如实传播

- **WHEN** webui vitest 阶段失败
- **THEN** 容器跳过其后的 e2e 阶段，退出码非零，输出目录中的 vitest 阶段日志可定位失败用例

#### Scenario: 交互口子可用

- **WHEN** 以 `docker run -it <镜像> bash` 进入容器
- **THEN** 获得可用 shell，工具链（cargo/node/pnpm/playwright）齐备可手动复现问题

### Requirement: 宿主机属主归还

验收过程写入挂载仓库的文件（`target/`、`webui/node_modules/`）与输出目录，在运行方提供 `HOST_UID`/`HOST_GID` 时 SHALL 于退出前将属主归还为该 UID/GID；未提供时 SHALL 正常完成验收。归还失败 MUST NOT 掩盖验收结果（验收成败决定退出码），但 SHALL 在日志中留下明确告警。

#### Scenario: 属主归还

- **WHEN** 宿主机用户 UID 1000 以 `HOST_UID=1000`、`HOST_GID=1000` 运行验收容器
- **THEN** 容器退出后挂载仓库内的 `target/`、`webui/node_modules/` 与输出目录中无 root 属主残留

#### Scenario: 不提供属主也可用

- **WHEN** 不传 `HOST_UID`/`HOST_GID` 运行验收容器
- **THEN** 验收流程照常完成，退出码语义不变
