// 确定的大输出：`node src/bigout.ts [MiB]`，默认 3 MiB。内容与 rust-mini 的 bigout 逐字节相同。
// 用 writeSync 而不是 process.stdout.write：输出接到管道时后者是异步的，
// 进程提前退出会丢掉还在缓冲里的尾巴，而尾巴正是要检查的东西。
import { writeSync } from "node:fs";

// 管道可能一次只收一部分，或者是非阻塞的（EAGAIN）：写到全部落地为止。
function writeAll(fd: number, text: string): void {
  const bytes = Buffer.from(text, "utf8");
  let offset = 0;
  while (offset < bytes.length) {
    try {
      offset += writeSync(fd, bytes, offset);
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "EAGAIN") throw error;
    }
  }
}

const mib = Number(process.argv[2] ?? "3") || 3;
const total = mib * 1024 * 1024;
const encoder = new TextEncoder();
const size = (text: string) => encoder.encode(text).length;

const head = "P57-EARLY-MARKER\n";
const mid = "P57-MIDDLE-MARKER\n";
const tail = "P57-LATE-MARKER 结束了，中文收尾。\n";

let chunk = head;
let written = size(head);
let middleDone = false;
let line = 0;
while (written < total - size(tail)) {
  if (!middleDone && written >= Math.floor(total / 2)) {
    chunk += mid;
    written += size(mid);
    middleDone = true;
    continue;
  }
  const text = `line ${String(line).padStart(7, "0")} 这是一行确定的填充文本 abcdefghijklmnopqrstuvwxyz\n`;
  chunk += text;
  written += size(text);
  line += 1;
  if (chunk.length > 65536) {
    writeAll(1, chunk);
    chunk = "";
  }
}
writeAll(1, chunk + tail);
writeAll(2, "P57-STDERR-MARKER\n");
