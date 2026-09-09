# 分支与发布策略：Rust main

本次调整仅整理分支和后续发布入口，不发布新版本，不重写主分支历史，不删除历史 Release。

## 三条长期分支

| 分支 | 定位 | 后续发布 |
| --- | --- | --- |
| `main` | 唯一主线；根目录 Rust 工程；新功能、性能和安全改进的默认目标 | 唯一正式 Release 来源 |
| `ts-port` | TypeScript legacy/reference；保留源码和回滚入口 | 不再作为正式 Release 来源 |
| `python-reference` | Python legacy/reference；保留原有 v0.2.0 参考实现和回滚入口 | 不再作为正式 Release 来源 |

不再长期维护单独的 `rust-port`。Rust 就是 `main`，不是第四条语言分支。
开发时可以临时创建功能分支；“三条分支”指长期分支，不禁止正常 PR 工作流。
新的同仓库 `codex/*` 分支在合并进 `main` 后，会先归档再自动清理；有新提交、保护规则或其他未关闭 PR 引用时保留。
其他临时分支在合并后由维护者清理，不自动删除不认识的分支。

## 这次清理与恢复

清理工作流先校验默认分支、固定 SHA、保护状态、开放 PR，以及两个 Codex 分支与 `main` 的祖先关系。
通过全部预检后，先创建并核对下表对应的 `archive/2026-09-rust-main/<分支名>` 标签，再删除三个旧分支指针。
旧 `rust-port` 已与主线分叉：只保存它的历史，不强行合并其旧实现，也不拿它覆盖现有 `main`。

| 审计时分支 | 固定提交 | 处理 |
| --- | --- | --- |
| `main` | `29a965267f81725962b1481c3a7e01288ddd0d6e` | 留存调整前快照；继续正常提交 |
| `ts-port` | `192d0b362de0df47478fc31cbfb8199761cc5e8c` | 分支保留；另留快照 |
| `python-reference` | `578b32ace77bbc732462b4d6483f2259969668ef` | 分支保留；另留快照 |
| `codex/add-diving-kitten` | `f5f04773e868206e96e10c3c7a6e8b6f11903e5f` | 已进入 main，归档后删除分支 |
| `codex/line-compat-update` | `eb743099a66836f1a1ec3d848f5164f1b9958105` | 已进入 main，归档后删除分支 |
| `rust-port` | `bfda7956e0dd7c4ac3d55ce7ae7f6d845f191f6b` | 旧迁移历史，归档后删除分支 |

恢复示例（仅在确需恢复旧迁移分支时执行）：

```bash
git fetch origin --tags
git switch -c rust-port refs/tags/archive/2026-09-rust-main/rust-port
git push -u origin rust-port
```

工作流不会绕过分支保护；出现新的提交或未完成 PR 时停止，重新审计后才能继续。
重跑清理工作流不会覆盖归档标签，也不会删除固定名单之外的分支。

## 分阶段退出 TS / Python

### 第一阶段：本次落实

- `main` 是唯一正式开发和发布主线；两条 legacy 分支保留，不改名、不强推。
- 新 Release 资产仅允许四个平台的 Rust 二进制与 `SHA256SUMS`，不上传 wheel、sdist、npm/TS 包或 Python 安装包。
- 保留 main 中现有 Python 参考源码、安装兼容层及差分测试；发布构建中的 Python 安装仅服务于跨运行时锁测试，不是 Rust 用户的运行时依赖。
- 保留历史 Release、历史 tag 和已经下载的旧版本；不撤回既有产物。
- 现有 Python 修复 PR #6 保持开放，不未经审查合并、关闭或重定向；兼容期内按修复本身评审。

### 第二阶段：后续独立变更，不在本次直接删除

将 main 中仍有用的 Python 兼容修复同步到 legacy 维护线，并迁移 Rust 依赖的共享工具契约和测试夹具。
补齐 Rust 独立契约测试后，再从发布构建中去掉 Python 测试依赖；保留独立的 legacy 对照测试任务。
最后从 main 移除 Python server、兼容安装器和不再使用的旧运行时代码，并同步修改所有安装、回滚和开发文档。
不得为了消除 Python 字样而直接跳过现有跨运行时锁及差分测试。

GitHub 自动生成的 `Source code (zip/tar.gz)` 是 tag 的源码快照；在第二阶段完成前，它仍可能含有兼容源码。
“Rust-only Release 资产”不等于“仓库和自动源码归档中已没有 Python”。

## 发版规则

1. 在 `main` 更新 `Cargo.toml` / `Cargo.lock` 的版本及 `docs/releases/vX.Y.Z.md`，先完成 CI。
2. 从 main 中的目标提交创建匹配包版本的 `vX.Y.Z` 或 `vX.Y.Z-prerelease` 标签。发布工作流会验证祖先关系、Rust 根工程、版本一致性和对应的发布说明。
3. 发布经过测试的 Rust 二进制及校验和。预发布标签标记为 prerelease，不冒充稳定版。

手动运行 Release 工作流只能选择 `main`，只构建 Actions artifacts，不自动创建 tag 或 GitHub Release。
不要在 legacy 分支打 `v*` 标签，也不要为 legacy 分支重新引入自动发布工作流。
祖先关系检查是当前发布工作流的防误操作约束，不是仓库级 tag 保护规则；维护者仍应通过仓库规则控制历史提交的标签创建。
归档标签使用 `archive/*`，与正式版本标签分离。
