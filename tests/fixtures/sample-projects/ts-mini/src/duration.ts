// 解析 `90s`、`1500ms`、`2m` 这样的时长，返回毫秒。
// 只用 Node 能直接剥掉的类型写法（不用 enum、namespace），所以不需要装 TypeScript。
// SCALE 这一行前后不留空行：task/02 补丁的上下文里有空行时，那一行就是单个空格，会被 git diff --check 当成行尾空白。
const SCALE: Record<string, number> = { ms: 1, s: 1_000, m: 60_000 };
export function parseDurationMs(text: string): number {
  const match = /^(\d+)([a-z]+)$/.exec(text.trim());
  if (!match) {
    throw new Error(`not a duration: ${JSON.stringify(text)}`);
  }
  const scale = SCALE[match[2]];
  if (scale === undefined) {
    throw new Error(`unknown unit ${JSON.stringify(match[2])}`);
  }
  const value = Number(match[1]) * scale;
  if (!Number.isSafeInteger(value)) {
    throw new Error(`${JSON.stringify(text)} overflows`);
  }
  return value;
}
