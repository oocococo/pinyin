# 第三方软件声明

pinyin 本身采用 MIT License。源码与便携包还使用或分发若干第三方组件；这些组件继续适用各自的许可证，pinyin 的 MIT License 不替代它们。

## 主要组件

| 组件 | 用途 | 许可证 | 上游项目 |
| --- | --- | --- | --- |
| librime | 本地中文输入引擎 | BSD-3-Clause | <https://github.com/rime/librime> |
| rime-prelude | Rime 基础配置 | LGPL-3.0 | <https://github.com/rime/rime-prelude> |
| rime-essay | Rime 共享词汇与语言模型 | LGPL-3.0 | <https://github.com/rime/rime-essay> |
| rime-luna-pinyin | 朙月拼音方案 | LGPL-3.0 | <https://github.com/rime/rime-luna-pinyin> |
| rime-pinyin-simp | 袖珍简化字拼音方案 | Apache-2.0 | <https://github.com/rime/rime-pinyin-simp> |

librime 的 Homebrew 构建还依赖 Boost、Cap'n Proto、gflags、glog、ICU、LevelDB、marisa-trie、OpenCC、yaml-cpp 等项目。实际便携包会根据本次构建真正复制的动态库生成 `BUILD-MANIFEST.txt`，并在 `LICENSES/` 中附带相应许可证原文。

Rust 依赖及其准确版本记录在 `Cargo.lock`。发布包的 `LICENSES/rust/` 目录会收集参与构建的 crates 所附许可证文件。

如发现许可证材料遗漏或归属错误，请通过 <https://github.com/oocococo/pinyin/issues> 报告。
