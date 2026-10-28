## 1. 规范文本

- [x] 1.1 按 delta（`specs/branding/spec.md`）更新 `openspec/specs/branding/spec.md` 三个 requirement，并直接改写其 Purpose 行的旧名提法为代称（Purpose 不走 delta）；验证：`grep -rniE "xkeeper|\bxk\b" openspec/specs/branding/spec.md` 零命中
- [x] 1.2 `openspec/specs/service-registration/spec.md` 场景示例 `--name xk-prod` → `--name ak-prod`（2 处，符号引用随命名契约更新）；验证：该文件 grep 旧名零命中
- [x] 1.3 `README.md` 下载链接 `cupen/xkeeper` → `cupen/appkeeper`（4 处，Linux curl ×2 + Windows Invoke-WebRequest ×2）；验证：`grep -n "cupen/" README.md` 全部为 cupen/appkeeper

## 2. src 与 tests 临时标识归并

- [x] 2.1 `src/{action,client,config,daemonize,main,metrics,program,pump,registry,server,service,supervisor,transport,web}.rs` 中 `#[cfg(test)]` 的 `xk-<场景>-` 临时目录/标识前缀全部归并为 `ak-test-<场景>-`（pid/tag 组合保持，不弱化并发唯一性）；验证：`cargo test --workspace` 单测段全绿
- [x] 2.2 `tests/revamp_integration.rs` 前缀与 sun_path 长度注释同步更新（`/tmp/ak-test-revamp-it-<tag>-<pid>` 需仍在 ~107 字节限内）；验证：`cargo test --test revamp_integration` 过
- [x] 2.3 `src/` 内夹具中性数据改 ak 形态：`service.rs` 的 `svc-xk`/`xk-prod_1.service`/`xk.service` 断言、`action.rs` 的 `/var/log/xk`、`main.rs` 的 `xk-definitely-not-an-editor`；验证：相关单测断言同步更新且全绿

## 3. xtask 与 bench 前缀联动

- [x] 3.1 `xtask/src/{e2e,stress}.rs`：`xk-xtask-*` → `ak-xtask-*`、mock editor 写入的 `/tmp/xk-e2e-*.sock` → `ak-xtask-` 域、bench 残留扫描 `strip_prefix("xk-bench-")` → `"ak-bench-"`（三处联动之一）；验证：`cargo build -p xtask` 过 + grep 该目录旧名零命中
- [x] 3.2 `appkeeper-bench/src/topology.rs`：workspace 前缀 `xk-bench-` → `ak-bench-`（生成侧，三处联动之二）及 no-daemon 测试路径；验证：`cargo test -p appkeeper-bench --lib` 过
- [x] 3.3 `appkeeper-bench/src/{api,cases,dcfg,integrity,report}.rs` 测试临时目录归并 `ak-test-`、`dcfg.rs` 中性数据 `C:\xk\logs` → `C:\ak\logs`；验证：`cargo test -p appkeeper-bench --lib` 全绿
- [x] 3.4 `appkeeper-bench/tests/integration.rs` 清理断言前缀 `xk-bench-` → `ak-bench-`（三处联动之三）与注释同步；验证：`cargo test -p appkeeper-bench --test integration` 过
- [x] 3.5 `scripts/verify-release-workflow.sh` 的 `GITHUB_REPOSITORY`/`--repo` 夹具值 `cupen/xkeeper` → `cupen/appkeeper`（3 处）；验证：`bash -n` 语法过 + grep 旧名零命中

## 4. webui 标识字样

- [x] 4.1 `webui/src/app-shell.ts` logo mark `xk` → `ak`，随后 `pnpm exec tsc --noEmit && pnpm test && pnpm build`，再 `cargo build` 重新嵌入；验证：三连全绿且 `grep -rn "xk" webui/src/` 仅剩无关命中（零命中）

## 5. 整体验证

- [x] 5.1 零残留门禁：`grep -rniE "xkeeper|\bxk\b" . --exclude-dir={.git,target,node_modules}` 排除 `openspec/changes/` 后零命中（等价命令：`grep -rniE "xkeeper|\bxk\b" src/ tests/ xtask/ appkeeper-bench/ scripts/ webui/src/ README.md AGENTS.md openspec/specs/ conf/ examples/ .ansible/ .github/`）
- [x] 5.2 `cargo test --workspace` 全绿（归并前缀后无并发/唯一性回归）
- [x] 5.3 `cargo run -p xtask -- e2e` 全绿（bench firehose 冒烟的清理断言按新前缀扫描）
- [x] 5.4 `openspec validate --change purge-legacy-identifiers` 与 `openspec validate --specs` 双绿
