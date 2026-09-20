# P36–P44 第一次在 Linux 上跑（2026-09-20）

P36 到 P44 加的十一个工具，此前**只在 macOS arm64 上跑过**。这一轮把中立 MCP 客户端测试搬到 Debian 13 / x86_64，用 v0.8.0 的**发布产物**、以专用低权限身份跑：**194 passed / 1 skipped**。

**结论先说**：产品代码一行没改。露馅的是测试自己的两个假设——一个 BSD 语义的 `mktemp`，一个"跑测试的账号是开发者账号"的前提。

## 1. 环境

| | |
| --- | --- |
| 机器 | hpsrv，Debian GNU/Linux 13 (trixie)，x86_64，内核 6.12.101+deb13-amd64 |
| glibc | 2.41（release notes 里量出来的下限就是按这个环境说的） |
| 身份 | `ccrun`，uid 1002，**只在自己的组里**，没有 sudo——P12 建的那个专用执行身份，一直还在 |
| 二进制 | `ccnm-0.8.0-linux-x86_64.tar.gz`，**CI 在 ubuntu-24.04 上构建的发布产物**，不是本机编的；sha256 与 release 里的 `.sha256` 对得上 |
| 其他 | Python 3.13.5、git 2.47.3、**ripgrep 14.1.1**（不是 macOS 上验 P37 时用的 15.2.0） |

**为什么用发布产物而不是本机编的**：hpsrv 上没有 Rust 工具链，而这正好让这一轮顺带验了发布流程——别人下载到的就是这个文件。

## 2. 怎么复现

本机下载并校验（hpsrv 直连 GitHub 慢，先下到本机再 scp）：

```bash
curl -sL -o ccnm-linux.tar.gz \
  https://github.com/xwfe/ccnm/releases/download/v0.8.0/ccnm-0.8.0-linux-x86_64.tar.gz
curl -sL -o ccnm-linux.sha256 \
  https://github.com/xwfe/ccnm/releases/download/v0.8.0/ccnm-0.8.0-linux-x86_64.tar.gz.sha256
shasum -a 256 ccnm-linux.tar.gz   # 和 .sha256 里那行比
```

装到 `ccrun` 名下（在 Runtime 上以 root 做，只碰它的 home）：

```bash
tar xzf ccnm-linux.tar.gz
install -D -m 755 ccnm /home/ccrun/.local/bin/ccnm
chown -R ccrun:ccrun /home/ccrun/.local
```

测试要四个目录：`tests/`、`scripts/`、`clients/`、`docs/{protocol,plan}`。**少一个都会当成失败**——`clients/` 少了是 `ModuleNotFoundError: ccnm_machine_client`，`docs/protocol` 少了是 25 个 `FileNotFoundError`，两种都看着像 Linux 上坏了，其实是没传全。

```bash
su - ccrun -c 'cd ~/ccnm-linux-check && CCNM_BIN=$HOME/.local/bin/ccnm python3 -m unittest discover -s tests'
```

## 3. 结果

```text
Ran 194 tests in 21.7s
OK (skipped=1)
```

其中 `tests/test_remote_workspace_mcp.py` 的 **36 条**是 P36–P44 的核心（含 P42 的三条生命周期、P44 的四条参数校验），单独跑 11.0 秒全过。

## 4. 露馅的两个测试假设

**一、`mktemp -d -t ccnm-p12` 是 BSD 写法。** GNU 的 `-t` 要求模板自带至少 6 个 `X`，拿到 `ccnm-p12` 直接报

```text
mktemp: too few X's in template ‘ccnm-p12’
```

退出码 1，于是 `DogfoodToolTests` 的每个用例都在 `setUp` 里挂掉——**32 个用例一次都没跑起来**。改用 `tempfile.mkdtemp`。原来用 shell `mktemp` 是为了拿真实路径（凭据检查对"路径可达性未知"是 fail-closed，而 macOS 的 `/var` 是符号链接），`resolve()` 照样管这件事。

这和 P12 那次的 `date -r` 是同一类：**测试助手里混进了只有 BSD 认的写法，在 macOS 上永远看不见。**

**二、`test_the_identity_audit_refuses_the_developers_own_account` 的前提在真 Runtime 上不成立。** 它的注释写着"跑测试的账号在 admin/staff 里，正是它要拦的那种"，然后断言审计必须失败。而 `ccrun` 没有 sudo、不在特权组里，**审计本来就该放行**，于是 `assertNotEqual(0, 0)` 红了。

这不是缺陷，反过来是件好事：**它证明 ccrun 确实被 ccnm 的身份审计认定为合格的执行身份**。现在遇到这种账号就 skip 并说明原因——它验的是"审计拦得住开发者自己的账号"，不是"审计总是拒绝"。

## 5. 这一轮没覆盖的

- **Managed 入口**（Controller / session / `ccnm run`）在 Linux 上仍未验收。Controller 是 launchd LaunchAgent，Linux 上不跑；这一轮只验 Runtime 那一半。
- **真实模型**没参与。这是中立客户端（不 import 任何 ccnm 代码、手拼 JSON-RPC）对真实二进制跑，不花额度，也不说明模型会不会用这些工具——那一条在[真实模型第一次用上这批工具](real-machine-p36-p44-2026-09-20.md)里，而且那一轮是 macOS。
- **Codex 当 Host** 没试。
- **Rust 那套测试**（`cargo test --workspace`）没在这台机器上跑，因为它没有 Rust 工具链。CI 的 ubuntu job 每次 push 都跑那套，但那是 ubuntu-24.04 不是 Debian 13。
- `exec_sandbox`、原生链这些依赖 bubblewrap / Codex 的路径没碰。
- **rg 14.1.1 只是"跑通了"，没有逐条比对它和 15.2.0 的行为差异**。P37 那些搜索语义是在 15.2.0 上测出来的，这一轮只证明测试在 14.1.1 上也过，不等于两个版本行为一致。
