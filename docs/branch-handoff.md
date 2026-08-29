# Git 分支交接

盘点日期：2026-08-30（Asia/Shanghai）。

## 当前状态

- 工作分支：`master`。
- `master` 比 `origin/master` 超前 7 个本地提交，尚未推送；这些提交包含 v0.1 实现、文档、lock 校验和 MCP Registry 安全校验。
- 本地工作区干净；`docs/goal/` 与 `target/` 仍按策略忽略。
- 已执行 `git remote prune origin`；收尾时删除了用户确认无用的远端 Dependabot 分支。

## 远端分支

本次已删除 9 个无用的 `dependabot/cargo/*` 与 `dependabot/github_actions/*` 分支，保留 `origin/master`。后续若出现新的废弃分支，可按以下步骤清理：

```text
git fetch --prune origin
git branch -r --merged origin/master
git push origin --delete <已合并且确认不再需要的分支>
```

删除前必须确认没有活动 PR 或自动化任务依赖该分支。不要删除 `origin/master`。

## 收尾步骤

1. 在审查确认后推送本地 `master`：`git push origin master`。
2. 按依赖 PR 的实际结果更新 [CHANGELOG](../CHANGELOG.md) 和 [实现状态](implementation-status.md)。
3. 发布前运行全量格式、测试、clippy 和跨平台 release workflow。
4. 仅删除已合并/关闭且确认无用途的远端分支，并保留本文件作为审计记录。
