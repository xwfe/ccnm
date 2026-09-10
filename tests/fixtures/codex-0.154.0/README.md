# codex-cli 0.154.0：适配器当前要求的版本

按[支持矩阵](../../../docs/support-matrix.md)的五步流程测出来的。差异比对写在
[0.154.0 研究记录](../../../docs/research/codex-0.154.0-2026-09-10.md)，Code Mode 那次改动写在
[P7.5 记录](../../../docs/research/p7-5-freeze-2026-09-10.md)。

```text
version.json / help.json / exec-help.json / auth-help.json / features.json   inspect，不启动模型
auth-current.json / auth-empty-home.json                                     登录状态，只有状态行，无凭据
seven-tools.{json,stdout}          真跑一次，七个工具全走一遍 —— 这份是 ccnm 现在实际会发的启动
tool-surface.{json,stdout}         只问模型看得见哪些工具，不让它动手
seven-tools-code-mode.{json,stdout} 同一个 CLI、同一个模型，但 Code Mode 被硬开着 —— 已不再产生
```

采集命令（`--model` 是必要的，原因见 0.154.0 记录第四节）：

```bash
python3 scripts/measure_codex.py <新目录> seven-tools --model gpt-5.3-codex-spark
```

```bash
python3 scripts/measure_codex.py <新目录> tool-surface --model gpt-5.3-codex-spark
```

读这份 fixture 前要知道的四件事：

- **`code_mode` 字段说明这次测的是哪种启动。** ccnm 只对实测过 Code Mode 的模型开它，目前那就是"不写 `model`"的 CLI 默认模型；`gpt-5.3-codex-spark` 明确不支持，所以 `seven-tools.json` 里 `code_mode` 是 `false`。两种配置的工具面差别见支持矩阵那张表。
- **`seven-tools-code-mode.*` 留着是有用的**，它是这次改动的证据：同一个模型被硬开 Code Mode 时，`apply_patch` 猜错四次才写对（`all_tools_succeeded` 因此是 `false`），而关掉之后七个工具一次全过。它记录的启动 ccnm 已经不再产生，别拿它当当前行为。
- **`runtime_file` 是 `CCNM_RUNTIME_PATCHED_7319\n\n`，两个换行。** 是模型自己多写的：它把不含换行的 `CCNM_RUNTIME_SENTINEL_7319` 换成了带换行的新值，原来那个换行还在。ccnm 写的就是收到的那条 edit。两次独立测量都是这样，所以采集脚本的严格判据在这一项上是不通过的——**保留原样，不为了变绿放宽判据**。
- 默认模型的形状由 `../codex-0.153.4/` 覆盖，那个目录是回归基线，不要动。
