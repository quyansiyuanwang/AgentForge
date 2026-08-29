# v0.1 实现状态与交接

本文档是当前代码状态的交接清单。`docs/goal/` 中的规格是目标基线；下面的状态以仓库实际代码、测试和 workflow 为准，状态变化时应同步更新。

## 已完成

- ProjectSpec 数据模型、Draft 2020-12 JSON Schema、语义校验和稳定 Diagnostic code。
- Detector：Git、Docker、GitHub Actions、语言/框架/数据库、包管理器、测试和常用工具；保留 evidence/confidence，单个 detector 失败不丢弃其他结果。
- Source adapter：local、HTTPS URL、Git/GitHub、skills.sh 兼容引用、官方 MCP Registry；redirect 限制、固定版本、SHA-256、lock/vendor 和离线校验。
- 安全扫描：路径穿越、绝对路径、符号链接、hard link、特殊文件、Windows 保留名、大小限制、可执行内容和敏感 URL/环境变量校验。
- Resolver、capability 决策、DesiredPlan、manifest、漂移/冲突检测、stale preview 和事务回滚/journal 恢复。
- Generic、Codex、Claude Code、Copilot CLI/cloud-manual renderer 及 golden snapshots。
- CLI/TUI：init、detect、sync、diff、doctor、config、skill、mcp、subagent、target；JSON 包络、非交互授权和 strict 模式。
- 跨平台 CI/release workflow 配置（Windows x64、Linux x64/arm64、macOS x64/arm64）。

## 部分完成或待完善

- **官方能力矩阵**：renderer descriptor 已绑定版本和 `verifiedAt`，但仍需在每次上游格式变化时复核官方文档并更新矩阵与 golden。
- **Doctor 环境检查**：当前会检查环境变量名称、Agent 可执行文件和 Codex trust 提示；尚未为每个 Agent 提供版本范围解析或自动修复建议。
- **TUI 验证**：已有 intent、取消、分页和常见尺寸快照；仍应补充真实终端 resize、diff 长文本和 Windows 控制台实测。
- **发布产物**：workflow 会构建和校验哈希，但尚未在本地验证五个平台的最终归档，也没有安装脚本或包管理器发布。
- **依赖治理**：workspace 已固定 `Cargo.lock` 并声明 Apache-2.0；尚未引入 `cargo-deny`/SBOM 生成，第三方许可证审计仍需发布前人工完成。

## 明确待实现（不应伪装成 v0.1 已交付）

- 真实远程 registry 的 scheduled integration 测试和录制 fixture 管理。
- 磁盘已满、权限在 apply 中途变化、进程崩溃等全部 fault-injection 场景的跨平台覆盖。
- 真实 Codex、Claude Code、Copilot 安装版本的 smoke test；当前 renderer 测试使用格式快照和本地桩。
- TUI 交互流程的端到端自动化（包括 terminal capability 差异）。
- 签名/证明、SBOM、组织 policy、私有 registry、动态插件执行、云账户/遥测、已有 Agent 配置导入和双向同步；这些属于 v0.1 非目标或后续路线。

## 交接入口

1. 修改 schema 或模型后，先更新 `schemas/project.schema.json`、语义 validator 和 `crates/core/tests/spec_validation.rs`。
2. 修改 source adapter 时，保持普通 `sync/diff/doctor` 无网络；更新 lock/vendor/offline 与安全测试。
3. 修改 renderer 时，更新 descriptor 的版本/日期/capabilities、golden snapshot、路径冲突测试和 [架构文档](architecture.md)。
4. 修改写入流程时，运行事务 fault tests，确认 journal、backup、manifest 顺序和失败回滚。
5. 提交前运行：

   ```text
   cargo fmt --all -- --check
   cargo test --workspace --locked
   cargo clippy --workspace --all-targets -- -D warnings
   git diff --check
   ```

`docs/goal/` 是本地规格草案并被 `.gitignore` 忽略；交接时以本文件和可追踪代码为准，不要将该目录加入提交。
