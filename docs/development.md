# 开发与发布

## 验证

```text
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets -- -D warnings
git diff --check
```

测试应覆盖 schema、detector fixtures、source lock/vendor/offline、四类 renderer golden、重复 sync、漂移/冲突、安全拒绝和事务回滚。当前 GitHub Hosted Runner 覆盖 Windows x64、macOS arm64、Linux x64/arm64；macOS x64 暂不纳入自动门禁，需使用自托管 runner 或本地交叉编译补验。

## 发布

版本号位于 workspace `Cargo.toml`。发布前更新 CHANGELOG、运行全量门禁并确认文档命令与 clap 帮助一致。release workflow 构建五个平台并生成 `SHA256SUMS.txt`；不得在 init、sync、doctor 中执行第三方脚本。

## 兼容性

每个 renderer 维护 Agent 版本和 capability 矩阵。新增或变更输出路径必须更新 manifest golden、冲突测试和迁移说明；不提供已有 Agent 配置自动导入，只报告冲突并给出人工迁移指引。
