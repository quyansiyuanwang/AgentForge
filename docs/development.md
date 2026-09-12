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

版本号位于 workspace `Cargo.toml`，发布流程按顺序执行：

1. 更新 CHANGELOG（为该版本建立带日期的 `## [x.y.z]` 小节）、运行全量门禁并确认文档命令与 clap 帮助一致。
2. `cargo build --release -p agentforge-cli` 本地打包测试版，在全新目录中对一个示例项目跑全流程（`init --non-interactive`、`detect`、`doctor`、`sync`、`diff`、资源增删、漂移检测），全部通过后才允许发布。
3. 将变更经 PR 合入 master，然后推送与 workspace 版本一致的 tag（如 `v0.1.0`）触发 release workflow。

release workflow 会先校验 tag 与 `Cargo.toml` 版本一致且 CHANGELOG 存在对应小节（并从中提取 release notes），再构建四个平台（Windows x64、Linux x64/arm64、macOS arm64）并生成 `SHA256SUMS.txt`；tag 含连字符（如 `v0.2.0-rc.1`）时自动标记为 pre-release。不得在 init、sync、doctor 中执行第三方脚本。

## 兼容性

每个 renderer 维护 Agent 版本和 capability 矩阵。新增或变更输出路径必须更新 manifest golden、冲突测试和迁移说明；不提供已有 Agent 配置自动导入，只报告冲突并给出人工迁移指引。
