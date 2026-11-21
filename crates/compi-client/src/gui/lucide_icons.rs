//! Every Lucide icon (github.com/lucide-icons/lucide 1.43.0, ISC, some MIT from Feather;
//! the licence is `assets/lucide/LICENSE`), unchanged, for the icons you choose for a
//! project. They draw in the text colour around them, at `lucide/<name>.svg` in the
//! application's assets.
//! The GitHub outline comes from Lucide 0.468.0.

include!(concat!(env!("OUT_DIR"), "/lucide_icons.rs"));

/// Where the icons come from and their licence, for the licences Settings shows.
pub(super) const ATTRIBUTION: &str = concat!(
    "LUCIDE (project icons, unchanged)\n",
    "Source: https://github.com/lucide-icons/lucide/tree/1.43.0/icons\n",
    "GitHub icon: https://github.com/lucide-icons/lucide/blob/0.468.0/icons/github.svg\n\n",
    "GitHub icon copyright (c) Lucide Contributors 2022.\n\n",
    include_str!("../../assets/lucide/LICENSE"),
);

/// What the icon search offers before you type: icons that suit kinds of projects.
const SUGGESTED: [&str; 24] = [
    "folder-code",
    "code-xml",
    "terminal",
    "globe",
    "server",
    "database",
    "bot",
    "sparkles",
    "smartphone",
    "monitor",
    "cloud-cog",
    "package",
    "book-open",
    "flask-conical",
    "shield-check",
    "rocket",
    "gamepad-2",
    "music",
    "image",
    "shopping-bag",
    "git-branch",
    "workflow",
    "wrench",
    "layers",
];

fn index(name: &str) -> Option<usize> {
    ICONS.binary_search_by(|(icon, ..)| (*icon).cmp(name)).ok()
}

/// The icon called `name` (`rocket`), by its path in the application's assets.
pub(super) fn path(name: &str) -> Option<&'static str> {
    index(name).map(|index| ICONS[index].1)
}

/// The icon file at `path`, when it is one of these.
pub(super) fn file(path: &str) -> Option<&'static [u8]> {
    let name = path.strip_prefix("lucide/")?.strip_suffix(".svg")?;
    index(name).map(|index| ICONS[index].2)
}

/// The icons whose names hold `query` (spaces read as dashes, any case), as names and
/// paths: the one it names first, then those starting with it, then the rest, each
/// alphabetically. An empty query gives the suggested ones.
pub(super) fn search(query: &str) -> Vec<(&'static str, &'static str)> {
    let query = query
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
        .to_lowercase();
    if query.is_empty() {
        return SUGGESTED
            .iter()
            .filter_map(|name| index(name).map(|index| (ICONS[index].0, ICONS[index].1)))
            .collect();
    }
    let mut found: Vec<_> = ICONS
        .iter()
        .filter(|(name, ..)| name.contains(&query))
        .map(|(name, path, _)| (*name, *path))
        .collect();
    found.sort_by_key(|(name, _)| (*name != query, !name.starts_with(&query)));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(query: &str) -> Vec<&'static str> {
        search(query).into_iter().map(|(name, _)| name).collect()
    }

    #[test]
    fn every_lucide_icon_is_bundled_in_name_order() {
        assert!(ICONS.windows(2).all(|pair| pair[0].0 < pair[1].0));
        assert_eq!(path("rocket"), Some("lucide/rocket.svg"));
        assert!(file("lucide/square-terminal.svg").is_some_and(|svg| svg.starts_with(b"<svg")));
        assert_eq!(path("no-such-icon"), None);
        assert_eq!(file("icons/rocket.svg"), None);
        assert!(SUGGESTED.iter().all(|name| path(name).is_some()));
    }

    #[test]
    fn search_finds_icons_by_any_part_of_their_name() {
        assert_eq!(names("  "), SUGGESTED.to_vec());
        assert_eq!(search("rocket")[0], ("rocket", "lucide/rocket.svg"));
        let terminal = names("Terminal");
        assert_eq!(terminal[0], "terminal");
        assert!(terminal.contains(&"square-terminal"));
        assert!(names("folder code").starts_with(&["folder-code"]));
        let arrow = names("arrow");
        let first_inner = arrow
            .iter()
            .position(|name| !name.starts_with("arrow"))
            .unwrap();
        assert!(
            arrow[first_inner..]
                .iter()
                .all(|name| !name.starts_with("arrow"))
        );
        assert!(search("zzzz").is_empty());
    }
}
