# codex-cli 0.154.0：适配器当前要求的版本

按[支持矩阵](../../../docs/support-matrix.md)的五步流程测出来的，差异比对与行为变化写在
[研究记录](../../../docs/research/codex-0.154.0-2026-09-10.md)里。

```text
version.json / help.json / exec-help.json / auth-help.json / features.json   inspect，不启动模型
auth-current.json / auth-empty-home.json                                     登录状态，只有状态行，无凭据
seven-tools.{json,stdout,stderr}                                             真跑一次，七个工具全走一遍
```

采集命令（`--model` 是必要的，原因见研究记录第四节）：

```bash
python3 scripts/measure_codex.py <新目录> seven-tools --model gpt-5.3-codex-spark
```

两件读这份 fixture 时要知道的事：

- **`all_tools_succeeded` 是 `false`**，因为模型猜了四次 `apply_patch` 的参数形状才写对，每次都被 ccnm 按名字拒绝。那是契约在起作用，不是回归；判据没有为了变绿而放宽，契约属性由 `crates/ccnm-core/tests/codex_measurements.rs` 单独钉住。
- 这一份是用 `gpt-5.3-codex-spark` 测的（默认模型当时没额度）。默认模型的形状由 `../codex-0.153.4/` 覆盖，那个目录是回归基线，不要动。
