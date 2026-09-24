# 文档导航

README 负责说明产品、上手与关键边界；本目录保存详细用法、契约、运维和维护证据。文档默认中文，README 开头保留英文简介。

| 读者与目的 | 阅读顺序 |
| --- | --- |
| 初次使用 | [快速开始](getting-started.md) → [配置](configuration.md) → [使用](usage.md) |
| 接入真实项目 | [生命周期与职责](project-lifecycle.md) → [支持矩阵](support-matrix.md) → [生产安全](production-safety.md) |
| 升级、清理或断线恢复 | [运维](operations.md) → [故障排查](troubleshooting.md) |
| 编写外部客户端或编排器 | [公开协议](protocol/README.md) → [执行接口交接](orchestrator-handoff.md) |
| 维护 ccnm | [架构](architecture.md) → [开发与发布](development.md) → [计划入口](plan/README.md) |
| 了解缺口与下一步 | [2026-09-23 审计](research/2026-09-23-lifecycle-and-docs-audit.md) → [状态账本](plan/status.json) |
| 实施手机远程操作（待实施） | [移动端总纲](plan/mobile-access.md) → [手机 SSH](plan/mobile-ssh.md) / [浏览器终端](plan/mobile-web-terminal.md) |

## 哪份文档回答哪种事实

`plan/status.json` 是阶段状态的唯一账本；`plan/ROADMAP.md` 定义阶段范围与验收条件。`support-matrix.md` 描述具体能力的支持范围，`protocol/` 定义公共契约，`research/` 保存带日期和环境的历史证据。阶段勾选不能代替能力验收，历史测试总数不能写成当前 HEAD 的事实，仓库中的发布记录也不是对用户机器安装状态的实时检查。

修改工具、入口、默认值或信任边界时，同步使用说明、配置、支持矩阵、相关协议与 README；修改收尾、输出保留或恢复逻辑时，同步运维和排错。不要通过覆写历史测量让旧证据看起来覆盖了新代码。

当前链接检查验证文件是否存在，不验证所有标题锚点、远端链接或文字与代码的含义是否一致；这些仍须审查，检查命令见[开发与发布](development.md)。
