# macOS 原生钥匙串验收

先将最终候选变更提交，在 Mac 的干净源码 checkout 上构建并运行，记录完整 commit revision。入口会检查构建时源码目录的 Git 根、HEAD 和工作树；未提交改动、未跟踪源码或无法确认状态都会在创建测试 Keychain 前非零退出，正常忽略的 `target/` 产物不影响检查。

第一阶段只使用临时 Keychain 和合成凭据。Linux mock 回归检查的是应用契约，不是 macOS 认证 UI。

## 第一阶段：隔离 Keychain 验证

```bash
# 在最终候选的干净 checkout 中执行；不要把 dirty delta 的结果归属旧 HEAD
revision=$(git rev-parse HEAD)
cargo build --locked --example macos_keychain_probe
mkdir -p target/reports/keychain
# 在 Mac 桌面交互终端运行；填写完整 revision，结果保留到日志
cargo run --locked --example macos_keychain_probe -- \
  --revision "$revision" --interactive \
  > target/reports/keychain/native-macos.jsonl
```

入口创建唯一临时 Keychain，合成密码为 `synthetic-keychain-test-password-only`。仅锁定、解锁、删除这个临时 Keychain，不更改 login Keychain 的锁状态、默认 Keychain 或个人条目。`security` 仅创建 ACL 受限的合成项及锁定、删除临时文件；不通过它读取任何 Key。

验收顺序：

1. 已有可读合成项连续启动三个独立 MCP 进程，均及时握手并报告 `system_store`。
2. 锁定临时 Keychain，再启动三次，均及时握手并报告 `access_required`，不得出现窗口。
3. 解锁临时 Keychain，显式授权并核验无交互读取，再启动三次应恢复可用。
4. 创建仅信任 `/usr/bin/false` 的合成项，再启动三次，必须无窗口并报告 `access_required`。
5. 工具提示后，第一次**显式授权**点 Deny；命令应一次失败，之后三次后台启动仍不得弹窗。
6. 第二次**显式授权**点 Always Allow。如系统要求密码，使用上述合成密码，不能输入 login 密码。命令应先完成交互读取，再完成无交互复检；之后三个独立进程应均可读取。
7. 删除临时 Keychain，输出 revision、产品版本、主机架构及结果。

任何非预期认证窗口、五秒内无法握手或状态不符都算失败。若误出现 login Keychain 认证，请取消并停止验收。失败时清理 guard 会尝试删除临时 Keychain；进程被强杀时，用工具路径所在临时目录定位其 `micu-native-acceptance.keychain-db`，仅删除该测试 Keychain。

省略 `--interactive` 可以运行无人值守的原生后台场景，但会以非零退出码明确报告交互恢复未验证。在非 Mac 主机运行同样非零退出，不生成“通过”结论。验收依赖原生 Mac 桌面会话；OSaka 不能替代这一步。

## 第二阶段：实际产品 binary 与客户端配置验证

隔离 helper 的授权身份不同于最终 `micu-image-mcp` 产品 binary，第一阶段通过后仍需在 Mac 上完成以下实际入口验收。全程不提交收费生图请求，不输出真实 Key、完整客户端配置或环境变量。

1. 从同一最终提交的干净 checkout 原生构建产品：`cargo build --locked --release --all-features`。记录完整 revision、架构、产品版本、构建产物路径和最终客户端启动路径，确认要安装的是本次原生构建的确切 binary。
2. 保留现有 binary 的可执行回滚副本及客户端配置备份。先断开该 MCP，再只替换其配置指向的产品 binary，不运行会重写配置的 `install`。保留原有客户端 env，尤其是 `MICU_KEYCHAIN_SERVICE`、`MICU_KEYCHAIN_ACCOUNT`；如果客户端通过 wrapper 启动，确认 wrapper 最终执行的就是这个 binary。失败时断开 MCP 后恢复旧 binary 与原配置。
3. 用**真实客户端配置**启动一个新的 stdio 连接，只调用 initialize、tools/list、server_info。核对版本、五工具和 `credentials.status`，不输出 Key；已有 Key 仅经正常产品 loader 读取。连续断开、重连至少三次，正常后台连接不得出现认证窗口，并留意原来每隔几分钟触发弹窗的重连场景。
4. 若状态是 `access_required`，由用户在终端中显式运行**同一已安装产品 binary** 的 `credentials authorize`，沿用客户端配置的相同 env 和自定义 service/account，不将真实 Key 放进 argv、日志或聊天。出现系统授权时只授权该程序。拒绝应一次失败退出，不自动重试；需要恢复时由用户重新发起一次显式操作。随后再次建立真实配置的新 stdio 连接，并重复至少三次重连，确认 `system_store` 且无后台弹窗。
5. 若状态是 `environment`，应继续使用原有环境 Key、不读取钥匙串；不要为验收删除环境 Key 强制触发授权。`missing` 或 `unavailable` 应保留该实际结果并排查配置/服务，不将其记为“已有钥匙串 Key 已恢复”。

实际阶段不得锁定 login Keychain、为制造拒绝反复修改真实条目 ACL，或要求永久允许所有 app。保留原有访问控制；预期授权窗口只能来自用户主动执行的凭据命令。发现任何后台弹窗、错误 binary/版本或无法恢复的凭据状态时停止替换并按保留副本回滚。

记录第一阶段和第二阶段各自的实际结果、版本/revision、执行主机及未完成项。目前尚无 Mac 执行入口：原生构建、隔离 UI 验收及最终产品身份的真实配置验收均未完成。

## 机制依据

现有 `apple-native-keyring-store` 的 macOS 后端使用 User-domain file Keychain 与 `SecKeychainFindGenericPassword`。本次保持同一 API、domain 和条目选择，只通过 `security-framework` 的安全 Rust API 在调用前建立 `SecKeychain::disable_user_interaction` guard；禁止 UI 失败则直接返回，不继续读取。进程内所有凭据读写串行；已被禁用的 UI 状态不会被 guard 恢复成启用。

- [security-framework 安全 API](https://docs.rs/security-framework/3.7.0/security_framework/os/macos/keychain/struct.SecKeychain.html#method.disable_user_interaction)
- [Apple：SecKeychainSetUserInteractionAllowed](https://developer.apple.com/documentation/security/seckeychainsetuserinteractionallowed(_:))

Linux 的既有 Secret Service 适配器仍可能调用 `ensure_unlocked`；本轮没有重写 Linux 后端，`macos_background_ui` 字段只描述 macOS。
