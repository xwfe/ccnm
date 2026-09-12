//! Which language ccnm speaks to a person, and how wide that language is
//! on a terminal.
//!
//! # What this is not for
//!
//! Only text a **person** reads goes through here: the doctor table, the
//! run reports, the guidance `ccnm init` prints. Three other audiences
//! read ccnm's output and none of them may ever see a translation:
//!
//! - **Frozen contracts.** `ccnm.machine/1` and `ccnm.workspace-mcp/1`
//!   carry `CCNM_E_*` names, JSON-RPC codes and enum words that other
//!   programs branch on. [`crate::ssh`] itself parses a remote stderr's
//!   first line back into an [`crate::ErrorCode`].
//! - **The model.** MCP tool descriptions and error bodies are
//!   instructions a coding agent acts on, verified in English on the P7,
//!   P11 and P12 rounds. Translating them changes measured behaviour with
//!   nothing going red.
//! - **Other people's programs.** ccnm matches English from `git`, `ssh`,
//!   `tmux` and Codex. This is why the language must never be selected by
//!   setting `LANG`/`LC_ALL` for child processes: that would silently
//!   break those matches — a root check degrading to "unknown", a login
//!   state that cannot be read — with no error anywhere.
//!
//! # Why a `match` and not an i18n crate
//!
//! Two languages, translated by the person who writes the code. A phrase
//! table bought with a dependency would add a runtime lookup that fails
//! by returning the key, where a `match` fails to compile. It also has to
//! be a parameter rather than a global: `cargo test` runs this crate's
//! tests in one process on many threads, and `set_var` is both `unsafe`
//! (forbidden workspace-wide) and unsound there.

use std::fmt;

/// The language a rendered report is written in.
///
/// [`Lang::En`] is the default at this layer on purpose. Core renders
/// English unless told otherwise, so the several hundred unit tests that
/// assert on rendered text keep asserting what they always did, and the
/// choice of what a person sees is made once, at the CLI entry point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lang {
    /// 中文，ccnm 对人说话的默认语言。
    Zh,
    /// English.
    #[default]
    En,
}

impl Lang {
    /// Parse a language name, accepting the spellings people actually
    /// type and the ones a `LANG`-shaped value carries (`zh_CN.UTF-8`).
    ///
    /// Unknown names are `None` rather than a silent fallback: someone
    /// who typed `--lang de` asked for something ccnm cannot do, and
    /// answering in English as if nothing happened hides that.
    pub fn parse(text: &str) -> Option<Lang> {
        let head = text
            .split(['_', '-', '.'])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        match head.as_str() {
            "zh" | "chinese" | "cn" => Some(Lang::Zh),
            "en" | "english" => Some(Lang::En),
            _ => None,
        }
    }

    /// The name this parses back from, for config files and messages.
    pub fn name(self) -> &'static str {
        match self {
            Lang::Zh => "zh",
            Lang::En => "en",
        }
    }

    /// Pick between two already-written strings. The whole translation
    /// layer is this function plus the call sites that use it.
    pub fn pick<T>(self, zh: T, en: T) -> T {
        match self {
            Lang::Zh => zh,
            Lang::En => en,
        }
    }
}

impl fmt::Display for Lang {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// How many terminal columns `text` occupies.
///
/// Rust's own `{:<24}` counts `char`s, so a CJK label — two columns per
/// character in every terminal — is padded as if it were half its width
/// and every column after it shifts right. That is invisible while the
/// output is ASCII, which is why it was never a bug before there was a
/// translation.
///
/// The ranges below are the East Asian Wide and Fullwidth blocks, which
/// covers what goes into a padded column: Han characters and fullwidth
/// punctuation, all of it written in this repository.
///
/// What it does *not* cover is the Ambiguous class — `—`, `…`, `·`, the
/// curly quotes — which a CJK-configured terminal draws two columns wide
/// and this counts as one. ccnm does print those (`——` appears in a
/// couple of sentences), so the rule is: they are fine in running text,
/// but a string that goes through [`pad`] must not contain them. Nothing
/// enforces that; it is a one-row misalignment if broken, which is also
/// why none of this justifies a dependency.
pub fn display_width(text: &str) -> usize {
    text.chars().map(char_width).sum()
}

fn char_width(c: char) -> usize {
    let c = c as u32;
    let wide = matches!(c,
        0x1100..=0x115F      // Hangul Jamo initial consonants
        | 0x2E80..=0x303E    // CJK radicals, Kangxi, CJK symbols (（）【】 etc.)
        | 0x3041..=0x33FF    // Hiragana, Katakana, Hangul Compatibility Jamo, CJK compat
        | 0x3400..=0x4DBF    // CJK Extension A
        | 0x4E00..=0x9FFF    // CJK Unified Ideographs
        | 0xA000..=0xA4CF    // Yi
        | 0xAC00..=0xD7A3    // Hangul syllables
        | 0xF900..=0xFAFF    // CJK Compatibility Ideographs
        | 0xFE30..=0xFE6F    // CJK Compatibility Forms, small form variants
        | 0xFF00..=0xFF60    // Fullwidth forms
        | 0xFFE0..=0xFFE6    // Fullwidth signs
        | 0x1F300..=0x1F64F  // Emoji that ccnm never prints, but a path might
        | 0x20000..=0x3FFFD  // CJK Extension B and beyond
    );
    if wide { 2 } else { 1 }
}

/// `text` followed by enough spaces to fill `columns` terminal columns.
///
/// Never truncates: a label wider than the column pushes the rest of its
/// row right, exactly as the old `{:<24}` did for a long English name. A
/// table row that is a little wide still reads; a name cut in half does
/// not.
pub fn pad(text: &str, columns: usize) -> String {
    let width = display_width(text);
    let mut out = String::with_capacity(text.len() + columns.saturating_sub(width));
    out.push_str(text);
    for _ in width..columns {
        out.push(' ');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip_and_locale_shaped_values_parse() {
        assert_eq!(Lang::parse("zh"), Some(Lang::Zh));
        assert_eq!(Lang::parse("EN"), Some(Lang::En));
        // A value copied out of a LANG variable, which is the shape
        // people paste even though ccnm never reads LANG itself.
        assert_eq!(Lang::parse("zh_CN.UTF-8"), Some(Lang::Zh));
        assert_eq!(Lang::parse("en-US"), Some(Lang::En));
        assert_eq!(Lang::parse(Lang::Zh.name()), Some(Lang::Zh));
        assert_eq!(Lang::parse(Lang::En.name()), Some(Lang::En));
        // Not a silent fallback: ccnm cannot do this, and has to say so.
        assert_eq!(Lang::parse("de"), None);
        assert_eq!(Lang::parse(""), None);
    }

    #[test]
    fn core_defaults_to_english_so_existing_renderings_are_unchanged() {
        assert_eq!(Lang::default(), Lang::En);
    }

    /// The bug this module exists to prevent, stated as an equality:
    /// four Chinese characters take the same columns as eight ASCII ones.
    #[test]
    fn chinese_is_two_columns_per_character() {
        assert_eq!(display_width("Config"), 6);
        assert_eq!(display_width("配置文件"), 8);
        assert_eq!(
            "配置文件".chars().count(),
            4,
            "which is what {{:<n}} counts"
        );
        // Mixed, which every real row is: a translated name next to an
        // untranslated tool or product name. The ASCII spaces around it
        // stay one column each.
        assert_eq!(display_width("远端 MCP 握手"), 4 + 1 + 3 + 1 + 4);
        assert_eq!(display_width("（）"), 4, "fullwidth punctuation too");
    }

    #[test]
    fn padding_fills_columns_not_characters() {
        assert_eq!(display_width(&pad("Config", 24)), 24);
        assert_eq!(display_width(&pad("配置文件", 24)), 24);
        assert_eq!(display_width(&pad("远端 MCP 握手", 24)), 24);
        // The two rows line up, which is the entire point.
        assert_eq!(
            display_width(&pad("Config", 24)),
            display_width(&pad("配置文件", 24))
        );
    }

    #[test]
    fn a_name_wider_than_its_column_is_never_cut() {
        let long = "a name that is far wider than the column it lives in";
        assert_eq!(pad(long, 24), long, "no padding, no truncation");
        assert_eq!(
            pad("七个工具都在这里了吧不止", 8),
            "七个工具都在这里了吧不止"
        );
    }

    #[test]
    fn pick_chooses_by_language() {
        assert_eq!(Lang::Zh.pick("中文", "English"), "中文");
        assert_eq!(Lang::En.pick("中文", "English"), "English");
    }
}
