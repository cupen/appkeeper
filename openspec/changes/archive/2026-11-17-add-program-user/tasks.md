## 1. 配置模型与身份解析

- [x] 1.1 `ProgramRaw` 与 `ResolvedProgram` 增加 `user`、`group`、`umask` 字段（`ResolvedProgram`: `Option<UserSpec>` / `Option<GroupSpec>` / `Option<u32>`），均可序列化以进入 `def_hash`；验证：`cargo build` 通过，现有配置解析测试不受影响
- [x] 1.2 实现 `UserSpec`、`GroupSpec` 解析与 `umask` 十六进制/八进制转换（`getpwnam_r` / 数字 uid 回退 `getpwuid_r`、`getgrouplist`、`getgrnam_r`/`getgrgid_r`、八进制字符串校验），在 `resolve_program` 中调用；验证：单元测试覆盖账号/组的存在与不存在、数字 uid/gid、`umask = "0027"` 合法与 `"08"`/`"1000"`/整数非法
- [x] 1.3 Windows 分支：`#[cfg(windows)]` 下解析到任一非空身份字段即报错；验证：Windows CI 编译通过，单元测试断言错误消息覆盖 user/group/umask
- [x] 1.4 补充解析层单元测试：三个字段进入 `def_hash`（改值后哈希变化）、`group` 无 `user` 的解析结果、`umask` 与 `env` 共存；验证：`cargo test --workspace` 新增用例通过

## 2. 子进程降权与环境

- [x] 2.1 `Program::spawn` 中实现身份切换：有效身份与 daemon 完全一致时跳过；root 时注册 `pre_exec` 按 `setgroups -> setgid -> setuid` 执行；非 root 同用户换组时只 `setgid`，失败返回 errno；验证：单元/集成测试在 root 下断言 uid/gid/附加组，非 root 环境断言拒绝或只换组，其余自动跳过
- [x] 2.2 `umask` 与身份环境注入：声明 `umask` 时在 `pre_exec` 调 `umask(2)`；`HOME`/`USER`/`LOGNAME`/`SHELL` 仅在 `def.env` 未显式设置时注入；验证：单元测试断言子进程创建文件的权限、显式 `HOME` 不被覆盖、未设置的键取 passwd 值，且 daemon 日志文件权限不受影响
- [x] 2.3 保持进程组语义：确认 `process_group(0)` 与 `pre_exec` 注册顺序，身份切换后停止仍能整组收信号；验证：既有停止/进程树测试通过，`cargo test --workspace`

## 3. daemon 侧可行性预检与告警

- [x] 3.1 实现 `check_user_feasible`（规则见 design D3），返回不可行原因与 root 告警列表；验证：单元测试覆盖「euid 相同且组相同」「root 降权」「非 root 跨用户」「非 root 同用户换到已属组」「非 root 换到非属组」「目标 root 告警」
- [x] 3.2 在 `cmd_compose_up` 与 `cmd_adhoc_up` 的 staging 之前接入预检，失败映射为 `ComposeUpError::Validation` / `AdhocUpError::Validation`；验证：新增测试断言失败时无占位文件、无进程启动
- [x] 3.3 daemon 启动加载与重扫路径：不可行 app 剔除并 `warn!`，root 身份程序各告警一次；验证：单元测试或集成测试断言坏 app 被隔离、daemon 继续运行
- [x] 3.4 `ComposePreview` 增加 `warnings: Vec<String>` 并在 `/v1/compose/up/preview` 的 JSON 中输出；验证：server 单元测试断言 root 场景响应含 warnings、其他分类不受影响
- [x] 3.5 CLI 输出：`ak compose up` 将 warnings 打到 stderr；`ak validate` 对非 root 跨用户打印 warning（非致命）；验证：`ak validate` 手工/集成用例输出含预期提示，退出码不变

## 4. 验收与收尾

- [x] 4.1 xtask e2e 增加 compose 身份场景：未知账号/组 → 400 零副作用；非法 `umask` → 400；非 root 指定他人用户或非属组 → 预览与提交均 400；root 环境走降权 + 换组 + umask roundtrip（非 root 自动跳过并 warn）；验证：`cargo run -p xtask -- e2e` 通过
- [x] 4.2 全量验证：`cargo test --workspace`、`cargo run -- validate`、`cargo run -p xtask -- e2e` 均退出 0；顺带确认 debug/release 构建无 warning 回归
- [x] 4.3 文档一致性：确认 `openspec/specs/{configuration,process-management,control-plane}` 的 delta 与实现对齐，无未覆盖的 SHALL；验证：`openspec validate "add-program-user" --strict` 通过
