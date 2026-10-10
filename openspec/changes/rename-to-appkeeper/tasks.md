## 1. Rust 主包与 manifest

- [x] 1.1 `Cargo.toml`：package `name` 改 `appkeeper`、description 更新、workspace `members` 改 `["xtask", "appkeeper-bench"]`；`cargo metadata` 通过
- [x] 1.2 `src/main.rs`：clap `#[command(name = "ak", about = …)]`、`--config` 帮助里的平台默认路径、`start` 移除提示与其它错误/提示文案中的程序名改 `ak`；`cargo run -- --help` 与 `--version` 显示 `ak`
- [x] 1.3 `src/main.rs` 及 `src/*.rs`：默认 daemon 配置路径常量与注释（`/etc/xkeeper/daemon.toml`、`%APPDATA%\xkeeper\daemon.toml`）改 `appkeeper`；`/tmp/xkeeper/logs`、`/var/log/xkeeper` 示例改 `appkeeper`；`cargo test` 通过
- [x] 1.4 `src/config.rs`：app 配置默认文件名 `xkeeper.toml` → `appkeeper.toml`（含 v0.1 迁移输出名、生成模板头注释）、`log_dir` 缺省与校验文案；`cargo test` 通过
- [x] 1.5 `src/registry.rs`：目录分派时查找/报错的文件名、脚手架生成头注释里的命令名与文件名；`cargo test` 通过
- [x] 1.6 `src/service.rs`：`DEFAULT_UNIT_FILE` 改 `/etc/systemd/system/ak.service`、unit `Description` 用 `appkeeper`、`xkeeper: warning` 前缀改 `ak:`、全部测试夹具路径（`/usr/local/bin/xkeeper`、`/etc/xkeeper/daemon.toml` 等）同步；`cargo test` 通过
- [x] 1.7 `src/server.rs`、`src/client.rs`、`src/shell.rs`、`src/action.rs`、`src/platform.rs`、`src/assets.rs`、`src/web.rs`、`src/daemonize.rs`、`src/supervisor.rs`、`src/program.rs`、`src/pump.rs`、`src/health.rs`、`src/metrics.rs`、`src/api.rs`：残留程序名、提示符（`ak> `）、临时目录/提示串前缀（`xkeeper-web-`、`xkeeper: skipped` 等）改 `ak`；`cargo test --workspace` 通过
- [x] 1.8 `build.rs`：环境变量改 `AK_WEBUI_BUILD`（去掉 `XKEEPER_FRONTEND_BUILD` 回退读取）、`AK_WEBUI_STAMP`、占位文件 `.xkeeper-placeholder` → `.ak-placeholder`、占位页文案与头注释；`AK_WEBUI_BUILD=skip` 下 `cargo build` 通过且生成占位页

## 2. bench crate 与 xtask

- [x] 2.1 `git mv xkeeper-bench appkeeper-bench`，改 crate `name` 为 `appkeeper-bench` 与 description；`cargo build -p appkeeper-bench` 通过
- [x] 2.2 `appkeeper-bench/src/**`：`BENCH_PREFIX` 改 `ak-bench-`、临时 app/程序名、报告里的二进制名字段、错误文案；`cargo test -p appkeeper-bench` 通过
- [x] 2.3 `appkeeper-bench/tests/integration.rs`：断言中的 `xkeeper-bench-` 前缀与产物路径常量同步；集成测试通过
- [x] 2.4 `xtask/src/*.rs`：daemon 产物路径常量改 `target/debug/ak`、bench 产物 `appkeeper-bench`、`-p xkeeper-bench` 调用改 `-p appkeeper-bench`、临时 app 目录/夹具文件名改 `appkeeper.toml`；`cargo run -p xtask -- e2e --no-browser` 通过

## 3. Web 控制台

- [x] 3.1 `webui/src/styles/tokens.css`、`shared.ts`、`app.css`、`wa-overrides.css`：CSS 自定义属性 `--xkeeper-*` → `--ak-*`（定义与全部引用成对替换）；`pnpm exec tsc --noEmit` 通过
- [x] 3.2 `webui/src/**/*.ts`：自定义元素标签 `xkeeper-*` → `ak-*`（`customElements.define` 与各视图模板成对）、localStorage 键 `xkeeper:theme` / `xkeeper.refresh_ms` → `ak:theme` / `ak.refresh_ms`；`pnpm test` 通过
- [x] 3.3 `webui/index.html`：`<title>` 改 `ak console`、`<xkeeper-app>` → `<ak-app>`、预绘制脚本里的存储键与注释；`webui/package.json` name 改 `appkeeper-webui`；`webui/vite.config.ts` 的 `XKEEPER_WEBUI_DEV_BACKEND` → `AK_WEBUI_DEV_BACKEND`
- [x] 3.4 前端全量验证：`pnpm exec tsc --noEmit && pnpm test && pnpm build` 通过，`webui/dist` 内无旧名残留

## 4. 部署、配置样例与 Ansible

- [x] 4.1 `conf/daemon.toml`、`conf/app.toml`：默认路径、日志目录、校验命令示例与注释中的产品名；`cargo run -- validate` 通过
- [x] 4.2 `examples/daemon.toml` 与 `examples/demo-app/xkeeper.toml` → `git mv` 为 `appkeeper.toml`，内容与注释同步；`cargo run -- validate` 通过
- [x] 4.3 `git mv .ansible/roles/xkeeper .ansible/roles/appkeeper`，`.ansible/site.yml` 与 role 内 `xkeeper_*` 变量、unit/二进制/配置落位路径、`.ansible/README.md` 同步；`ansible-playbook --syntax-check`（或 `--list-tasks`）通过
- [x] 4.4 `.github/workflows/ci.yml`、`.gitignore`、`LICENSE`：检查并按需同步旧名（Copyright 行、忽略路径）；全仓搜索确认无命中

## 5. 文档与规范

- [x] 5.1 `README.md`：产品名改 `appkeeper`、全部命令示例改 `ak …`、默认路径与文件名改新名、新增"从 xkeeper 迁移"清单（命令名/env/配置文件/unit 名）
- [x] 5.2 `AGENTS.md`：标题与架构描述中的产品名、命令示例、`-p appkeeper-bench` 调用、构建说明中的 `AK_WEBUI_BUILD`
- [x] 5.3 `openspec/specs/**`（16 个能力规范）：产品名、命令名、路径、unit 名、crate 名按 `branding` 分层规则就地改写（不改写 requirement 语义）
- [x] 5.4 活跃变更 `openspec/changes/add-config-subcommand`（proposal/design/tasks/specs）中的命令与路径引用改新名，使其未完成任务不再写回旧名
- [x] 5.5 `openspec validate --specs` 通过

## 6. 全量校验与收尾

- [x] 6.1 零残留断言：`grep -ri xkeeper .`（排除 `openspec/changes/archive/`、`.git/`、`target/`、`webui/dist/`、`webui/node_modules/`）无命中
- [x] 6.2 `cargo build --release` 产出 `target/release/ak`（无 `xkeeper` 产物），`ak --version` 正常
- [x] 6.3 `cargo test --workspace` 在 unix 与 windows 两条路径通过（本地 + CI 矩阵）
- [x] 6.4 `cargo run -p xtask -- e2e` 全绿（含 playwright 浏览器段与 bench 冒烟），确认控制台标签/存储键改名后交互无回归
- [x] 6.5 `cargo run -p appkeeper-bench -- --case firehose --duration 10` 冒烟通过，报告内无旧名
