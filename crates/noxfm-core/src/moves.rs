//! Following items the daemon reports as renamed or moved (`Event::Moved`).

use std::path::{Path, PathBuf};

/// Where `p` is now, if it is one of the moved items (`(from, to)`) or inside one.
pub fn relocated(p: &Path, moves: &[(PathBuf, PathBuf)]) -> Option<PathBuf> {
    moves.iter().find_map(|(from, to)| {
        let rest = p.strip_prefix(from).ok()?;
        // `join("")` would add a trailing slash.
        Some(if rest.as_os_str().is_empty() { to.clone() } else { to.join(rest) })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relocation() {
        let moves = vec![(PathBuf::from("/d/new"), PathBuf::from("/d/old"))];
        assert_eq!(relocated(Path::new("/d/new"), &moves), Some("/d/old".into()));
        assert_eq!(relocated(Path::new("/d/new/a/b"), &moves), Some("/d/old/a/b".into()));
        assert_eq!(relocated(Path::new("/d/newer"), &moves), None, "a sibling with the same prefix");
        assert_eq!(relocated(Path::new("/d"), &moves), None);
    }
}
