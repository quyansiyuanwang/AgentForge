# Git 分支交接

盘点日期：2026-08-30（Asia/Shanghai）。

## 当前状态

- 工作分支：`master`。
- `master` 比 `origin/master` 超前 7 个本地提交，尚未推送；这些提交包含 v0.1 实现、文档、lock 校验和 MCP Registry 安全校验。
- 本地工作区干净；`docs/goal/` 与 `target/` 仍按策略忽略。
- 已执行 `git remote prune origin`，没有发现失效的远端跟踪引用。

## 远端分支

远端当前存在 9 个 Dependabot 分支，分别对应 GitHub 上仍处于 OPEN 状态的 PR #1–#9（Cargo 依赖和 GitHub Actions 更新）。这些分支均有明确审查对象，不属于可安全删除的无用分支：

`dependabot/cargo/*`、`dependabot/github_actions/*`

在对应 PR 合并或关闭前不要删除。PR 处理完后可按以下步骤清理：

```text
git fetch --prune origin
git branch -r --merged origin/master
git push origin --delete <已合并且确认不再需要的分支>
```

删除前必须确认 GitHub PR 已 merged/closed，且没有其他自动化任务依赖该分支。不要删除 `origin/master`。

## 收尾步骤

1. 在审查确认后推送本地 `master`：`git push origin master`。
2. 按依赖 PR 的实际结果更新 [CHANGELOG](../CHANGELOG.md) 和 [实现状态](implementation-status.md)。
3. 发布前运行全量格式、测试、clippy 和跨平台 release workflow。
4. 仅删除已合并/关闭且确认无用途的远端分支，并保留本文件作为审计记录。
