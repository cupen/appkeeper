## 1. 镜像基础层

- [x] 1.1 新建 `docker/Dockerfile.test`：`debian:bookworm-slim` 基座，apt 装
  `ca-certificates`、`curl`、`build-essential`（gcc + libc6-dev），建立 `/ak`
  HOME 与 `CARGO_HOME`/`PLAYWRIGHT_BROWSERS_PATH` 等缓存路径环境变量；`docker build`
  成功且镜像内 `curl --version`、`gcc --version` 可用
- [x] 1.2 新建 `.dockerignore`：白名单只放锁文件与 manifest（`Cargo.toml`/`Cargo.lock`/
  `xtask/Cargo.toml`/`appkeeper-bench/Cargo.toml`/`webui/package.json`/`webui/pnpm-lock.yaml`
  + `docker/`），其余全部排除；构建上下文缩到 KB 级
- [x] 1.3 实现 `REGION` build-arg（缺省 `global`）：Dockerfile 内对非
  `global`/`cn` 值直接报错列出可选值；映射 rustup/rsproxy、npm registry、
  Node tarball base、playwright 镜像、apt 镜像（cn 走 aliyun，且必须用 http——
  镜像层尚未安装 CA 证书）。验证 `REGION=eu` 构建失败且错误含 `global`/`cn` 字样

## 2. 工具链与预烘层

- [x] 2.1 工具链层：`RUST_VERSION=stable`（实测 1.99.1）、`NODE_VERSION=22.23.3`、
  `PNPM_VERSION=11.24.0`；REGION 只换 base URL，安装代码路径两线路相同。镜像内
  `cargo --version`、`node --version`、`pnpm --version` 与 `packageManager` 一致
- [x] 2.2 预烘层：`COPY` 根 `Cargo.lock` + 三份 `Cargo.toml` +
  `webui/{package.json,pnpm-lock.yaml}` 后执行 `cargo fetch --locked`（先以
  `cargo metadata --no-deps` 自检 manifest 集合）、`pnpm fetch`、
  `npx playwright@1.57.0 install --with-deps chromium`；镜像仅含 13 字节 stub 源文件
- [x] 2.3 验证 playwright 预装位置可被 e2e 解析：镜像内以 HOME=/ak 模拟
  `xtask/src/e2e.rs` 的 `$HOME/.npm/_npx` 解析逻辑，成功定位 playwright 1.57.0 且
  `chromium.executablePath()` 指向 `/ak/ms-playwright/chromium-1200/...` 且文件存在

## 3. 验收编排（entrypoint）

- [x] 3.1 新建 `docker/entrypoint.sh`：`set -o pipefail`，按 `AK_TEST_STAGES`
  （缺省 all）执行四段——`cargo test --workspace`、webui 三连（tsc/vitest/build）、
  `cargo run -p xtask -- e2e`、bench 报告段；每段 tee 到 `/out/<NN>-<stage>.log`，
  任一段失败即停；`bash` 入口直接进 shell
- [x] 3.2 webui 三连在 e2e 之前产出真实 `webui/dist`：e2e 浏览器段
  （`BROWSER PASS OK`）正常渲染控制台即为验证
- [x] 3.3 属主归还：提供 `HOST_UID`/`HOST_GID` 时退出前对 `/work/target`、
  `/work/webui/node_modules`、`/work/webui/dist`、`/out` 执行 `chown -R`，失败仅告警
  不改退出码；全量运行后 `find docker-out target webui/node_modules -user root` 为空

## 4. 宿主机包装与文档

- [x] 4.1 新建 `scripts/docker-test.sh`：封装 build（REGION/版本 build-arg 透传、
  tag `appkeeper-test:latest`）+ run（`-v $repo:/work`、
  `-v $repo/docker-out/<时间戳>:/out`、注入调用者 `HOST_UID/HOST_GID`、`--rm`）、
  退出码透传；执行后 `docker-out/<时间戳>/` 含四段日志与 bench JSON
- [x] 4.2 README 增「Docker 测试环境」段（build/run/REGION=cn/离线复跑/产物位置/
  属主归还/构建缓存隔离），AGENTS.md Build & Run 区补一行指引；文档中的命令逐一可执行

## 5. 全量验收（spec 场景逐条）

- [x] 5.1 `scripts/docker-test.sh` 全量跑通：三段套件 + bench 报告段全绿、退出码 0、
  `docker-out/20261019-161725/` 产物齐全（4 段日志 + `bench-firehose.json`）、
  宿主机无 root 属主残留（specs「全量通过」「属主归还」场景）
- [x] 5.2 线路与拒绝路径：`REGION=cn` 构建成功且镜像可用、`REGION=eu` 构建失败
  报可选值（specs「cn 线路构建」「未知 REGION 拒绝」场景）
- [x] 5.3 离线复跑：锁文件不变时 `docker run --network none` 全量验收四段全绿、
  退出码 0（specs「离线全量验收」场景）。锁漂移场景按 design D5 由常规联网运行
  的自动回源覆盖（同一代码路径），未单独构造
- [x] 5.4 交互口子：`docker run -it appkeeper-test bash` 内 cargo/node/pnpm/
  playwright(chromium 路径可达) 齐备可手动复现（specs「交互口子可用」场景）

## 6. 实施中发现的必要修复

- [x] 6.1 `xtask/src/e2e.rs::curl_socket` 改用分离参数 `--unix-socket <path>`：
  等号语法自 curl 8.15 才被接受，bookworm 自带 7.88 会以 exit 2 拒绝，
  导致容器内 e2e 在 G0 起持续失败。容器内 e2e 全绿验证修复有效
- [x] 6.2 bench 段显式传 `--daemon target/debug/ak`：bench 按名 `appkeeper` 发现
  daemon，而 workspace 产物名为 `ak`。bench 段产出 JSON 报告验证修复有效
