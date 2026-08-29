# CLI 参考

全局二进制名为 `agentforge`。

| 命令 | 作用 |
| --- | --- |
| `init` | 检测项目、确认选择并创建 spec/lock/vendor 与生成物 |
| `detect` | 输出带 evidence/confidence 的项目事实 |
| `sync` | 根据已锁定来源编译并事务应用；默认离线 |
| `diff` | 比较 desired state 与工作树 |
| `doctor` | 校验 spec、来源、manifest、漂移和环境要求 |
| `config validate/show` | 校验或查看 ProjectSpec |
| `skill/mcp/subagent/target` | 管理对应资源或 target |

`init --non-interactive` 必须提供 `--target` 或已有 spec；非 Git 目录需 `--allow-non-git`。资源 `update` 才允许联网刷新，必要时使用 `--strict`、`--allow-unpinned-source` 或 `--allow-executable-content`。

退出码：`0` 成功；`1` 业务验证失败或 strict warning；`2` 参数用法错误；`3` 来源解析/网络错误；`4` 安全策略拒绝；`5` 应用事务失败并已回滚；`6` 文件系统或配置 IO 错误。
