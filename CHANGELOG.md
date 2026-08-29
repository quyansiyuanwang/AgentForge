# Changelog

遵循 Keep a Changelog 格式，版本遵循语义化版本。

## [Unreleased]

- 持续改进文档、检测器和 target capability 覆盖。

## [0.1.0] - 未正式发布

- 建立 ProjectSpec、JSON Schema、Detector/Resolver 和 ResolvedPlan。
- 支持 local、URL、Git/GitHub、skills.sh 兼容源及官方 MCP Registry，并提供 lock/vendor、SHA-256 与离线同步。
- 提供 Generic、Codex、Claude Code、GitHub Copilot renderer，含 manifest、漂移检测、冲突阻断和事务回滚。
- 提供 init、detect、sync、diff、doctor、config、skill、mcp、subagent、target 命令及 JSON 输出。
- 覆盖路径穿越、恶意符号链接、超限下载、未授权脚本和 secret 泄漏防护。
