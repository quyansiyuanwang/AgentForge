# 架构

AgentForge 固定执行单向编译：Detection + user selection -> `project.yaml` -> Resolve + Vendor -> `lock.yaml`/`vendor/` -> Desired State + renderers -> preview/transactional apply -> `manifest.json` -> doctor。

## 契约

ProjectSpec 包含 project、targets、instructions、skills、mcp、subagents、settings。来源是判别联合（local、url、git/github、skills.sh-compatible、official MCP registry）。通用设置使用规范化字段，target 专有字段必须通过 schema 校验的 `extensions.<target>` 提供。`generic` 只能单独选择。

Detector 只产生带证据和置信度的事实；Resolver 产生带理由的建议。TUI 必须确认建议，非交互模式不隐式接受。未支持能力默认 warning 并跳过，`--strict` 时阻止应用。

## 状态与事务

每个修改命令先生成相同的 ResolvedPlan 和 diff。应用阶段写入隔离 staging，完成验证后原子替换并记录 journal/backup；任一步失败都回滚。删除 spec 条目仅在 manifest 哈希未漂移时删除生成物，否则报告冲突。

## 扩展

新增 source adapter 需实现固定版本、哈希、离线和网络失败语义；新增 renderer 需声明 Agent 版本、capabilities、输出路径和验证器，并提供 golden snapshot。Application Service 是唯一写入入口，保证所有 CLI/TUI 行为一致。
