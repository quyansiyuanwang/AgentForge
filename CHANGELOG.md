# Changelog

遵循 Keep a Changelog 格式，版本遵循语义化版本。

## [Unreleased]

- 持续改进文档、检测器和 target capability 覆盖。

## [0.1.0] - 2026-09-12

### 新增

- 建立 ProjectSpec、JSON Schema、Detector/Resolver 和 ResolvedPlan。
- 支持 local、URL、Git/GitHub、skills.sh 兼容源及官方 MCP Registry，并提供 lock/vendor、SHA-256 与离线同步。
- 提供 Generic、Codex、Claude Code、GitHub Copilot renderer，含 manifest、漂移检测、冲突阻断和事务回滚。
- 提供 init、detect、sync、diff、doctor、config、skill、mcp、subagent、target 命令及 JSON 输出。
- 覆盖路径穿越、恶意符号链接、超限下载、未授权脚本和 secret 泄漏防护。
- release workflow：tag 与 workspace 版本一致性校验、从 CHANGELOG 提取 release notes、四平台打包与 `SHA256SUMS.txt`，`-rc`/`-beta` tag 自动标记 pre-release。

### 修复

- `--json` 在资源变更校验失败时保持机器可读输出，不再退回人类格式。
- `skill/mcp update` 遇到损坏的 `lock.yaml` 时报错退出（码 1），不再静默以空 lock 覆盖导致其余 vendor 条目丢失。
- `skill add <file>` 对非 `SKILL.md` 的本地文件源直接以用法错误（码 2）拒绝，替代必然失败的编译回滚路径。
- Git 子路径匹配改为按路径段比较，并以单元测试固化兄弟前缀目录的行为。
- lock 新增 `executablePaths`：跨平台离线校验可重放可执行位分类，Windows 不再因权限位缺失误报校验和不一致（旧 lock 仍可读取）。
- `doctor` 的 Agent CLI 版本探测增加 5 秒超时，Windows 下经 `cmd /C` 解析 npm shim，消除挂起与误报。
- `doctor` JSON 输出中 `mcpSecurity` 补齐 `healthy` 字段，`vendor` 检查不再无条件报告健康。
- `init --dry-run` 的冲突检测覆盖 skill/subagent 产物目录（`.agents/skills/**` 等）。
- 资源 id 推导统一：`add`/`remove`/`init` 均取末段路径、忽略大小写地剥离 `.md` 并小写化，可按添加时的原始值删除资源。

### 变更

- 退出码完整实现 `docs/cli.md` 契约：`3` 来源解析/网络错误，`4` 安全策略拒绝（vendor 校验、逃逸路径、超限、可执行内容），`5` 应用事务失败并已回滚，`6` 文件系统或配置 IO 错误。
- CI quality/test job 显式安装 Rust toolchain，clippy 增加 `--locked`。
- `SpecValidator` 只在构造时编译一次 JSON Schema；`planning::classify`、`resource_id` 等核心逻辑补充内联单元测试与公共 API rustdoc。
