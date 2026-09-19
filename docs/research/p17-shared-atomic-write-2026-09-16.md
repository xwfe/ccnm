# P17：原子写入接共享库（2026-09-16）

## 结论

- `apply_patch` 的两处落盘动作换成共享 crate `toexec-fs`（仓库 `toexec`）：
  `write_atomic_temp` 的四步交给 `toexec_fs::write_durable`，`commit_one` 里的
  `fs::rename` 换成 `toexec_fs::replace`。**行为逐字节不变**，`apply_patch` 的既有
  测试一条断言都没改。
- **错误的措辞和错误码原样保留**。这是这一刀里最容易做错的地方，见下一节。
- 临时文件命名（`TEMP_PREFIX`）、`sweep_stale_temps`、journal、备份、回滚编排
  全部没动。两边的回滚差得远（ccnm 按操作类型分并写 journal，gld 一刀切恢复），
  共享它们只会做出一个全是开关的东西。
- ccnm 这一侧**没有净收益**，收益在 gld 那边：它原来没有 fsync、不保留权限。
  ccnm 换来的是 `replace` 里的 Windows 分支（暂时用不上）和少一处重复。

## 差点改掉的协议行为

原来那个函数把四步各配一条消息，而且**错误码不一样**：

| 失败在 | 消息 | 错误码 |
| --- | --- | --- |
| `File::create` | `cannot write beside {rel}` | `invalid_args` |
| `write_all` | `cannot write {rel}` | `invalid_args` |
| `sync_all` | `cannot flush {rel}` | **`internal`** |
| `set_permissions` | `cannot set permissions on {rel}` | **`internal`** |

第一版共享 API 把四步合成一个 `io::Error`，接进来就变成了一条消息、一个
`invalid_args`。没有测试会因此失败（没有测试断言这几条消息），但它悄悄改了
两件事：模型看到的措辞，以及 MCP 错误码——`invalid_args` 是「你的参数有问题，
改了再来」，`internal` 是「机器的问题，改参数没用」。刷盘失败报成参数错误，
会把调用方支到错误的方向。

所以共享库改成返回 `WriteError { step, source }`，由调用方决定怎么归类。
ccnm 这边按上表逐条还原。共享 crate 因此发了 0.2.0（破坏性改动，0.1.0 已经
推出去了就不动它）。

**这条经验适用于后面每一刀**：抽公共机制的时候，错误的分类和措辞是产品的
对外契约，不是可以顺手统一的实现细节。

## 验证

macOS arm64，Rust 1.98：

| 门禁 | 结果 |
| --- | --- |
| `cargo fmt --all --check` / `clippy --workspace --all-targets -D warnings` | 通过 |
| `cargo test --workspace` | **719 passed / 0 failed**，与 P16 记录的数字一致 |
| `cargo test -p ccnm-cli --test external_mcp` | 22 passed |
| `python3 -m unittest tests.test_remote_workspace_mcp` | 10 passed（中立 MCP 客户端） |
| `check_plan` / `check_protocol` | 通过 |
| 共享 crate 自己 | 19 passed（`toexec-fs` 10 + `toexec-text` 9） |

`toexec-fs` 那 10 条里有三条故障路径：父目录不存在、临时文件不存在、目标是一个
目录（必须报错，而且不能把目录删掉）；还有一条钉死「替换后的权限跟着临时文件
走，不跟着目标走」——所以调用方必须在写的那一步就把原权限传进来。

## 没验的

- gld 那一侧还没接（另算一笔，在 gld 仓库记账）。
- **断电/崩溃时的持久性没有实测**。`fsync` 的作用是推理出来的，不是在真实掉电
  下量出来的；共享库文档里还记了一个已知边界：`rename` 这件事要在断电后仍然
  可见，得再 fsync 父目录，两个产品原来都没做，这一刀也没做。
- Windows 的 `replace` 分支没在 Windows 上跑过（ccnm 不支持 Windows；gld 支持，
  但它的 CI Windows job 只做 `cargo check`）。
  > **2026-09-19 后来的事**：那个分支已经删掉了——它先 `remove_file` 再 rename，
  > 后一步失败就把旧文件弄丢了（跨仓评审 X01）。现在所有平台都只做一次
  > `rename`，并且在 windows-latest 上真跑了测试。结果和仍然存在的边界（Windows
  > 上替换不了只读目标）见
  > <https://github.com/xwfe/toexec/blob/main/evidence/x01-windows-replace/README.md>。
  > ccnm 这边从 `toexec-fs-v0.2.0` 升到了 `v0.2.1`。
- 没有跑真机、没有换已安装的二进制、没有消耗模型额度。
