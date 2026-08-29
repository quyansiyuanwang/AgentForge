# AgentForge

AgentForge 是一个面向工程团队的 Rust 工具，把项目事实、用户选择和可审计的外部来源编译为 Codex、Claude Code、GitHub Copilot 及通用 Agent 配置。当前版本为 v0.1.0；重点是可重复、可离线、可回滚的配置交付。

## 快速开始

```text
cargo install --path crates/cli
cd your-repository
agentforge init --target codex --non-interactive --allow-non-git
agentforge doctor
agentforge sync
agentforge diff
```

首次 `init` 会生成 `.agentforge/project.yaml`、锁定来源并写入 vendor；后续普通 `sync` 只使用 lock/vendor，不联网。非交互模式必须显式指定 target 或提供已有 spec。

## 核心模型

`project.yaml` 是用户意图的唯一真源；`lock.yaml` 固定远程版本、最终 URL 和 SHA-256；`vendor/` 保存可离线重建的规范化内容；`manifest.json` 记录 renderer、路径和哈希。生成文件由 AgentForge 整文件托管，用户应修改 canonical source，而不是生成物。未托管文件或漂移文件会阻止覆盖并显示 diff；应用采用暂存、验证、原子替换和失败回滚。

## 命令

```text
agentforge init [--dry-run] [--non-interactive] [--strict] [--allow-non-git]
agentforge detect [--json]
agentforge sync [--dry-run] [--strict]
agentforge diff [--json]
agentforge doctor [--strict] [--json]
agentforge config validate [path]
agentforge config show
agentforge skill|mcp|subagent|target list|add|remove|update
```

退出码：`0` 成功，`1` 验证失败或 strict warning，`2` 参数用法错误；其余运行错误见 [CLI 文档](docs/cli.md)。

## 安全边界

远程包在隔离暂存区校验路径、符号链接、大小和哈希后才进入 vendor。AgentForge 不执行第三方脚本、MCP command 或 hook，也不读取或持久化 secret 值；preview/doctor 会显示命令、网络权限和所需环境变量名称。

## 开发

```text
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets -- -D warnings
```

架构、扩展点和发布流程见 [docs/README.md](docs/README.md)。项目规格草案位于本地 `docs/goal/`，按策略被 `.gitignore` 忽略，不作为发布文档。

## 许可证

AgentForge 采用 Apache License 2.0，见 [LICENSE](LICENSE)。第三方依赖和外部 skill/MCP 内容遵循其各自许可证。
