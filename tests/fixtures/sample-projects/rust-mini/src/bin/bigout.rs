//! 确定的大输出：`bigout [MiB]`，默认 3 MiB。
//!
//! 头、正中、尾各一个标记，用来检查结果链路有没有丢头丢中间；尾部带中文，
//! 用来检查按字节截断时会不会切坏字符。stderr 另写一行，检查两个流是否分开。
use std::io::Write;

fn main() {
    let mib: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(3);
    let total = mib * 1024 * 1024;
    let tail = "P57-LATE-MARKER 结束了，中文收尾。\n";
    let out = std::io::stdout();
    let mut out = std::io::BufWriter::new(out.lock());
    let mut written = 0;
    let mut middle_done = false;
    let mut line = 0u64;
    let head = "P57-EARLY-MARKER\n";
    out.write_all(head.as_bytes()).unwrap();
    written += head.len();
    while written < total - tail.len() {
        if !middle_done && written >= total / 2 {
            let mid = "P57-MIDDLE-MARKER\n";
            out.write_all(mid.as_bytes()).unwrap();
            written += mid.len();
            middle_done = true;
            continue;
        }
        let text = format!("line {line:07} 这是一行确定的填充文本 abcdefghijklmnopqrstuvwxyz\n");
        out.write_all(text.as_bytes()).unwrap();
        written += text.len();
        line += 1;
    }
    out.write_all(tail.as_bytes()).unwrap();
    out.flush().unwrap();
    eprintln!("P57-STDERR-MARKER");
}
