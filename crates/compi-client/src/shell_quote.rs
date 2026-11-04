//! Text typed into a shell as one argument.

use std::borrow::Cow;

/// `text` as one PowerShell argument: as it is when nothing in it is special to
/// PowerShell, else in single quotes. PowerShell also reads `‘ ’ ‚ ‛` as single quotes,
/// so those are doubled like `'`.
pub fn powershell(text: &str) -> Cow<'_, str> {
    let plain = !text.is_empty()
        && !text.starts_with('-')
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_./\\:+=%".contains(&byte));
    if plain {
        return Cow::Borrowed(text);
    }
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('\'');
    for character in text.chars() {
        if matches!(
            character,
            '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}'
        ) {
            quoted.push(character);
        }
        quoted.push(character);
    }
    quoted.push('\'');
    Cow::Owned(quoted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn powershell_quotes_only_what_it_must_and_doubles_every_single_quote() {
        assert_eq!(powershell(r"C:\Users\me\a.png"), r"C:\Users\me\a.png");
        assert_eq!(powershell("feature/x-1"), "feature/x-1");
        assert_eq!(powershell(r"C:\My Files\a b.png"), r"'C:\My Files\a b.png'");
        assert_eq!(powershell("it's"), "'it''s'");
        assert_eq!(powershell("it\u{2019}s"), "'it\u{2019}\u{2019}s'");
        for special in ["$HOME", "a,b", "@args", "a;b", "a`b", "-x", "(x)", ""] {
            assert!(powershell(special).starts_with('\''), "{special}");
        }
    }
}
