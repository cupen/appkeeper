## Why

全量验收（`cargo test` + webui 三连 + `xtask e2e` 含浏览器段）要求本机凑齐 Rust stable
（edition 2024）、Node ≥ 22、pnpm 11、playwright + chromium、curl 等一整套工具链，
bookworm 等 发行版仓库版本全部不够，环境不齐或版本漂移时验收口径不一致甚至跑不起来。
需要一个一次构建、跨 commit 复用的 Docker 化测试环境：`docker run` 一条命令全自动跑完
全量套件，退出码如实反映成败，报告落回宿主机。

## What Changes

- 新增 Docker 测试镜像（基座 `debian:bookworm-slim`）：预装 Rust（build-arg，默认
  stable）、Node 22、pnpm 11.24、playwright + chromium（含系统库）、curl、
  ca-certificates、build-essential——slim 自带的 sh/coreutils/util-linux(`script`)
  覆盖 e2e 其余外部命令依赖。
- 镜像构建期预烘依赖缓存：当前 `Cargo.lock` 的 crates 全量 `cargo fetch`、
  `pnpm fetch` 进 store、chromium 落浏览器缓存目录；运行期 lock 未变时离线可跑，
  漂移时回退联网拉取。
- 单一 `REGION` build-arg 切线路：默认 `global`（官方源），传 `cn` 统一切大陆镜像
  （rustup/crates→rsproxy，npm/playwright→npmmirror 镜像，Node 走二进制镜像而非
  NodeSource）。
- 一次性验收器入口：运行期 `-v` 挂载仓库到 `/work`，自动依次跑三段套件——
  `cargo test --workspace`、webui 三连（tsc/vitest/build）、`cargo run -p xtask -- e2e`
  （含浏览器段与 bench firehose 冒烟）；各阶段日志与 bench JSON 报告落 `/out`
  挂载卷，任一阶段失败退出码非零；保留 `docker run -it <img> bash` 交互口子。
- 宿主机属主归还：运行末按 `HOST_UID`/`HOST_GID` 把挂载仓库内 target/、
  webui/node_modules/ 与产物目录的属主交回宿主用户，避免 root-owned 残留。
- 配套 `.dockerignore` 与包装脚本、README 使用说明；不改 `.github/workflows/ci.yml`
  （CI 维持现状），不动业务代码。

## Capabilities

### New Capabilities

- `docker-test-env`: Docker 化测试环境的构建与运行契约——镜像依赖清单与工具链版本
  策略、REGION 线路切换、预烘缓存与离线运行、一次性全量验收的执行流程与产物/
  退出码契约、宿主机属主归还。

### Modified Capabilities

（无——本变更不改动任何既有能力的需求；CI、控制面、webui 行为均维持原样。）

## Impact

- 新增：`docker/Dockerfile.test`（或同级落位，见 design）、容器入口脚本、
  `scripts/` 下宿主机包装脚本、`.dockerignore`、README「测试环境」段落
  （AGENTS.md 构建命令区同步一句指引）。
- 另一处最小修复：`xtask/src/e2e.rs` 的 `curl_socket` 把
  `--unix-socket=<path>` 改为等价的分离参数形式——等号语法自 curl 8.15 才被
  接受，而 debian:bookworm 自带 curl 7.88，不修则容器内 e2e 无法运行。
- 不触碰：`src/`、`webui/src/`、`.github/`、`openspec/specs/` 既有规范。
- 运行边界：Linux 容器路径 only——Windows 平台专属行为不在本环境覆盖面内；
  `scripts/auth-browser-*.mjs` 等手动脚本不纳入套件。
- 镜像体积代价：含浏览器工具链约 1GB+（已确认接受，换取「全量」名副其实）。
