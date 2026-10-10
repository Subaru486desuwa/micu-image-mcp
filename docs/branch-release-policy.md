# 分支与发布策略：Rust main

`main` 是唯一开发、测试与正式发布主线，当前源码零 Python。`python-reference` 保留独立历史参考，不参与主线 CI。必要 Shell/PowerShell bootstrap 保留；不维护独立 rust-port 或 ts-port 产品线。

## TS 历史归档与恢复

两个精确 TS tip 的源码树相同，提交历史不同，必须分别保存：

| 来源 | 精确提交 |
| --- | --- |
| local ts-port | `0bae340e8d5ab01a906f3dac2b4234787e1841b5` |
| origin/ts-port | `192d0b362de0df47478fc31cbfb8199761cc5e8c` |

新增 history-only/ours 合并，以 Rust main 为第一父提交，两个 TS tip 为额外父提交，保持 Rust 源码树。先将归档提交持久推送并验证远端 main 的祖先关系，再清理 local/remote ts-port refs；删除前复核实际 tip、worktree、开放 head/base PR 和保护规则。并发变动或保护必须停止删除，不强推、不改写历史。TS 独有 `--mcp-name` 和旧模型/chat fallback 未迁入当前产品。

完整克隆可在需要时恢复任一精确历史：

```bash
git fetch origin main
# 浅克隆先执行 git fetch --unshallow origin
git switch -c ts-reference-local 0bae340e8d5ab01a906f3dac2b4234787e1841b5
# 或恢复远端当时的历史：
git switch -c ts-reference-remote 192d0b362de0df47478fc31cbfb8199761cc5e8c
```

main 祖先可达性是恢复证据，仅在文档写 SHA 不算持久归档。历史 Release/tag 不删除，旧 Python reference 分支不改变。既有 rust-port 的精确归档 `bfda7956e0dd7c4ac3d55ce7ae7f6d845f191f6b` 继续保留；不清理与本任务无关的 worktree 或本地分支。

## 临时分支清理

Rust `cargo xtask maintain-branches` 默认只读审计；`--apply` 才删除固定退休名单或同仓库已合并到 main 的 codex/* 事件分支。先审计整个计划，再逐项复核精确 tip、保护、开放 PR 和 main 祖先关系。TS 同时要求两个精确 tip 均已保留。未知分支不自动删除；squash/rebase 后原 tip 不可达时保留。

工作流从可信 main 执行 Rust 工具，不使用 inline JavaScript。GitHub API token 只从进程环境读取，不写日志。所有删除检查仍由远端保护规则约束，不扩大权限。

## 发版规则

只从 main 发布根 Rust package。tag 必须匹配 Cargo.toml 的 `vMAJOR.MINOR.PATCH[-prerelease]`，对应 `docs/releases/<tag>.md` 必须存在。校验由 `cargo xtask release-check` 完成；四个平台在原生 runner 完成 Rust build/test/STDIO smoke。

Release 只附带 Linux x86_64、macOS ARM64/x86_64、Windows x86_64 binary 和 SHA256SUMS。手动工作流只构建 artifacts，不自动创建 tag/Release。本次语言迁移不发布新版本，不删除或覆盖旧产物。
