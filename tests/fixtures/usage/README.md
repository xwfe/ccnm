# `ccnm usage` 的实测样本

`claude --print /usage --output-format json` 和 `codex app-server` 的 `account/rateLimits/read` 的真实输出，P105 的解析测试读它们。怎么采的、改了哪些字段（会话 id、账号 id、一个 skill 名）、各自的环境，见[研究记录第 3 节](../../../docs/research/2026-10-19-p105-usage.md#3-实测)。

不要为了让测试通过重录：输出变了先在研究记录里写新版本实测了什么，再加一份新文件。
