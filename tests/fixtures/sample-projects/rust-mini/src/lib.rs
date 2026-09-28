//! 解析 `90s`、`1500ms`、`2m` 这样的时长，返回毫秒。

/// 把带单位的时长解析成毫秒。数字必须是非负整数，单位必须写。
pub fn parse_duration_ms(text: &str) -> Result<u64, String> {
    let text = text.trim();
    let split = text
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(|| format!("missing unit in {text:?}"))?;
    let (number, unit) = text.split_at(split);
    let value: u64 = number
        .parse()
        .map_err(|_| format!("not a number in {text:?}"))?;
    let scale = match unit {
        "ms" => 1,
        "s" => 1_000,
        "m" => 60_000,
        _ => return Err(format!("unknown unit {unit:?}")),
    };
    value
        .checked_mul(scale)
        .ok_or_else(|| format!("{text:?} overflows"))
}
