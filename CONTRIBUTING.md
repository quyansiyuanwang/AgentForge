# 贡献指南

## 开发环境

需要 Rust 1.85 或更新版本。使用 workspace 根目录执行命令；提交前运行格式、测试和 clippy 门禁。

## 变更原则

- `project.yaml`、lock、vendor 和 manifest 的行为必须保持可重复且可离线。
- 新能力先更新 schema、核心模型和 renderer/source capability，再补测试与文档。
- target 专有选项放入类型化 `extensions.<target>`，禁止透传任意原生片段。
- 生成路径冲突、用户漂移和安全校验失败必须阻止应用，不得静默覆盖。

## 测试与提交

为 schema、detector、source adapter、renderer、事务回滚和安全边界添加回归测试。提交信息使用简短的命令式前缀（如 `feat:`, `fix:`, `docs:`）；不要提交 `.agentforge` 运行产物、`target/` 或 `docs/goal/`。

## 安全

测试中使用占位符而非真实凭据。AgentForge 在 init/sync/doctor 中不执行第三方脚本；发现漏洞请按 [SECURITY.md](SECURITY.md) 私下报告。
