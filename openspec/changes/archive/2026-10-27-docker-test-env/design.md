## Context

全量验收目前只能在本机凑齐 Rust stable + Node ≥ 22 + pnpm 11 + playwright/chromium + curl
的环境上跑（见 proposal.md - Why）。已核实的事实约束：

- bookworm 发行版仓库的 rustc 1.63 / nodejs 18 均低于本项目下限（edition 2024 需
  Rust ≥ 1.85，`build.rs` 要求 Node ≥ 22）——工具链必须外部安装。
- e2e（`xtask/src/e2e.rs`）直接 spawn 的外部命令：`curl`（硬依赖，至少 3 处）、
  `sleep`/`id`（coreutils）、`sh`（dash）、`script`（util-linux，RDF16 PTY 场景）、
  `node`（浏览器段）；后四者 bookworm-slim 自带。
- playwright 由 e2e 在运行期从 `$HOME/.npm/_npx/*/node_modules/playwright` 或
  `webui/node_modules/playwright` 解析——预装位置必须落在解析可见的路径上。
- debug 构建 rust-embed 运行时读盘：e2e 浏览器段要求 `webui/dist` 是真实前端构建
  产物，故三段编排中 webui build 必须先于 e2e。
- `.gitignore` 已覆盖 `/target`、`/webui/node_modules`、`/webui/dist`——容器写挂载
  仓库不产生 git 污染，只剩属主问题。

## Goals / Non-Goals

**Goals:**

- 干净机器上 `docker build` 一次 → `docker run` 全自动跑完全量套件，退出码即成败。
- `REGION=global|cn` 单开关覆盖全部下载线路，两线路产物功能等价。
- 锁文件不变时验收可离线复跑（`--network none` 亦通过）。
- 宿主机仓库无 root 属主残留、无 git 污染；报告落宿主机时间戳目录。

**Non-Goals:**

- 不改 `.github/workflows/ci.yml`：CI 维持 GH 托管双平台 `cargo test` 现状，
  本环境是本地/自建场景的全量验收补充，不承担 CI 执行器职责。
- 不覆盖 Windows 平台专属路径（Linux 容器是已知边界）。
- 不纳入 `scripts/auth-browser-*.mjs` 等手动脚本；不做 fmt/clippy 门禁。
- 不做多平台/多架构镜像矩阵（linux/amd64 优先，arm64 不专门验证）。

## Decisions

### D1 落位：`docker/Dockerfile.test` + `docker/entrypoint.sh` + `scripts/docker-test.sh`

镜像资产（Dockerfile、entrypoint、REGION→镜像源映射表）集中 `docker/`，宿主机包装
脚本放既有 `scripts/`。备选「根目录 Dockerfile」被否：与未来可能的运行时镜像混位；
备选「docker-compose」被否：单容器一次性运行用不上编排，compose 反而多一层间接。

### D2 工具链安装统一走「官方 tarball / rustup」单代码路径，REGION 只换 base URL

Rust 经 rustup-init（`RUSTUP_DIST_SERVER`/`RUSTUP_UPDATE_ROOT` 切线路）；Node 用
官方 tarball 解压安装（global=nodejs.org/dist，cn=npmmirror 的 node 镜像）；
pnpm 用 `npm install -g pnpm@$PNPM_VERSION`（registry 切线路）。备选「global 走
NodeSource apt、cn 走 tarball」被否：两条线路两种安装方式，等价性验证面翻倍；
统一 tarball 后 REGION 只是环境变量差异。crates 线路用 `~/.cargo/config.toml` 的
registry replacement（rsproxy）+ git CLI 不装（全 registry 依赖）。chromium 系统库
用 `playwright install --with-deps chromium` 让 playwright 自己 apt 装齐（root 可
用），避免手工枚举 libnss3/libgbm1 等随版本漂移的清单。

### D3 镜像内统一 HOME=/ak，预烘与运行同身份（root）

镜像新建 `/ak` 作为 HOME 与缓存根：`CARGO_HOME=/ak/cargo`、npm/npx 缓存
`/ak/.npm`、`PLAYWRIGHT_BROWSERS_PATH=/ak/ms-playwright`、pnpm store
`/ak/pnpm-store`。预烘（cargo fetch / pnpm fetch / npx playwright install）与
验收运行同为 root、同 HOME——e2e 对 playwright 的 `$HOME/.npm/_npx` 解析直接命中，
不引入「构建期用户 ≠ 运行期用户」的缓存不可见问题。备选「专用非 root 用户」被否：
运行期还要写挂载仓库（宿主 uid 未知）与 `/out`，非 root 用户反而需要 uid 透传 +
缓存 chown 两套机制。安全面可接受：测试镜像非分发物、不做长期驻留服务。

### D4 预烘即 Docker 层：COPY 锁文件 → fetch 三连，锁不变则层缓存命中

层序：apt 基础层 → 工具链层（受 build-arg 版本影响）→ `COPY Cargo.lock` +
三处 `Cargo.toml` + `webui/{package.json,pnpm-lock.yaml}` → `cargo fetch` +
`pnpm fetch` + `npx playwright install` 层。锁文件未变时预烘层全部缓存命中，
重建秒级；源码永不进镜像（跨 commit 复用）。`cargo fetch` 需最小可编译 manifest
集合（根 `Cargo.toml`、`xtask/`、`appkeeper-bench/` 三份 + 根锁文件），实现时以
`cargo metadata --no-deps` 自检 fetch 完整性。

### D5 离线复跑不靠 offline 开关，靠缓存命中天然零网络

运行期不加 `--offline`/`CARGO_NET_OFFLINE`：cargo 对已 fetch 的依赖直接用本地
缓存（不访问网络），锁漂移时自动联网补拉——两种情形一个代码路径。pnpm 用
`pnpm install --prefer-offline --frozen-lockfile`（store 命中优先，缺失才回源）。
`--network none` 下全部走本地缓存；loopback（webui 探测、playwright 连
127.0.0.1）不受 network namespace 影响。备选「显式 offline 开关 + 失败回退重跑」
被否：失败重跑整段浪费且逻辑分叉。

### D6 三段编排 + bench 报告收尾段，entrypoint 阶段化

`entrypoint.sh` 依次执行四段，每段输出 tee 到 `/out/<NN>-<stage>.log`，
`set -o pipefail` 保证 tee 不吞退出码，任一段失败即停：

1. `cargo test --workspace`（覆盖 xtask/bench 单测，比 CI 的根包口径更全）；
2. webui 三连：`pnpm exec tsc --noEmit`、`pnpm test`、`pnpm build`（产出真实
   `dist/`，供 debug rust-embed 运行时读盘——必须是 e2e 前置）；
3. `cargo run -p xtask -- e2e`（含浏览器段；e2e 自带 bench firehose 冒烟，但其
   JSON 报告随临时 workspace 销毁，不外导）；
4. bench 报告段：`appkeeper-bench --case firehose --duration <短> --json
   /out/bench-firehose.json`，兑现「输出目录含 bench JSON 报告」的产物契约。

支持 `AK_TEST_STAGES` 环境变量选段（默认 all），交互 `bash` 入口直接进 shell
不触发套件。备选「把 bench JSON 塞进 e2e 冒烟」被否：e2e 的临时 workspace 契约
是跑完即清（benchmark 规范），为外导报告破例得不偿失。

### D7 属主归还：退出前 chown 三处，成败码优先

entrypoint 捕获套件退出码后，若提供了 `HOST_UID`/`HOST_GID`，对 `/work/target`、
`/work/webui/node_modules`、`/out` 执行 `chown -R`，归还失败仅告警不改退出码。
`scripts/docker-test.sh` 默认注入调用者的 uid/gid，创建 `docker-out/<时间戳>/`
挂载为 `/out`，`docker run --rm` + 退出码透传。

### D8 `.dockerignore` 收紧构建上下文

排除 `.git`、`target`、`webui/node_modules`、`webui/dist`、`docker-out`、
`.ansible`、`openspec/changes/archive` 等；构建只需锁文件与 manifests，
上下文从仓库全量缩到 KB 级。

## Risks / Trade-offs

- [镜像 ~1GB+]（含 chromium 与工具链）→ 接受：一次性构建、本地复用，换来「全量」
  名副其实；Docker 层缓存使重建增量很小。
- [`chown -R target/` 在大缓存目录上耗时可达分钟级] → 仅在显式提供 HOST_UID 时
  执行；包装脚本文档说明可跳过（代价是 root 属主残留）。
- [playwright/chromium 版本随 `npx playwright install` 浮动] → 预烘命令钉
  `playwright@<版本>`（build-arg，缺省取 e2e 兼容的当前大版本），升级走显式
  build-arg 变更。
- [rustup/npm 镜像源可用性随时间漂移] → `global`/`cn` 双线路等价，任一线路不可
  用时可切另一线路重建，不阻塞验收。
- [与宿主机共用 `target/` 的指纹互踩（容器/宿主机交替构建触发重编）] → 接受：
  只损失编译时间不损失正确性；文档提示可用 `AK_TEST_TARGET=/work/target-docker`
  类变量隔离（实现时若有成本则记为后续项，不阻塞本变更）。
- [`cargo fetch` 的 manifest 集合与 workspace 演进不同步] → D4 的
  `cargo metadata --no-deps` 自检在构建期暴露缺漏，失败即构建失败。

## Migration Plan

纯新增（`docker/`、`scripts/docker-test.sh`、`.dockerignore`、README/AGENTS 文档
段落），不触碰业务代码与 CI。回滚 = 删除新增文件；镜像本身可 `docker rmi`。

## Open Questions

- 镜像 tag 策略（`appkeeper-test:latest` 之外是否带 REGION/工具链版本后缀）——
  实现时顺手定即可，不影响契约。
