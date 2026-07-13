# 参与贡献

感谢你愿意改进 pinyin。项目目前专注于 macOS 上“保持英文输入源，通过前缀临时输入中文”的体验。

## 开始之前

开发环境需要：

- macOS；
- Rust stable；
- Homebrew；
- librime。

```sh
brew install librime
git clone https://github.com/oocococo/pinyin.git
cd pinyin
bash scripts/download-rime-data.sh
```

## 提交修改

1. 先创建描述清楚的 Issue，或在现有 Issue 中说明准备解决的问题。
2. 保持一次提交只解决一个主题，避免混入无关格式化或重构。
3. 新行为应附带自动化测试；修复缺陷时优先先加入可复现测试。
4. 不要提交 `data/`、`dist/`、`target/`、日志或个人 Rime 用户数据。
5. 提交 Pull Request 时说明动机、行为变化、验证方式和已知限制。

提交前运行：

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
bash -n scripts/*.sh
bash scripts/test-conversion.sh
```

## 报告问题

请尽量附上 macOS 版本、芯片架构、系统输入源、目标应用、最短复现输入序列和实际结果。

`scripts/run-listener-debug.sh` 会记录详细键盘事件。日志可能包含按键和输入内容；公开上传前务必检查并删除密码、令牌、聊天内容及其他敏感信息。

## 许可证

提交代码即表示你同意按仓库的 [MIT License](LICENSE) 分发你的贡献。请勿提交来源不明、许可证不兼容或无权再分发的代码、词典与数据。
