//! Undo for file operations, shared by every window (like Explorer's Ctrl+Z).
//!
//! Undoing never deletes for good: copies, new items and links go to the
//! trash, so an undo can itself be recovered.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use noxfm_core::transfer::unique_target;

use crate::trash;

const DEPTH: usize = 20;

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Renamed { from: PathBuf, to: PathBuf },
    /// `(original, new location)` per item.
    Moved(Vec<(PathBuf, PathBuf)>),
    /// The copies made.
    Copied(Vec<PathBuf>),
    /// Trash ids.
    Trashed(Vec<String>),
    Created(PathBuf),
    Linked(Vec<PathBuf>),
}

fn name(p: &Path) -> String {
    p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned())
}

fn count(n: usize, one: &str, many: &str) -> String {
    if n == 1 { one.to_owned() } else { format!("{n} {many}") }
}

impl Action {
    /// "Undo <label>"
    pub fn label(&self) -> String {
        match self {
            Action::Renamed { from, .. } => format!("rename of “{}”", name(from)),
            Action::Moved(v) => format!("move of {}", count(v.len(), &format!("“{}”", name(&v[0].0)), "items")),
            Action::Copied(v) => format!("copy of {}", count(v.len(), &format!("“{}”", name(&v[0])), "items")),
            Action::Trashed(v) => format!("delete of {}", count(v.len(), "1 item", "items")),
            Action::Created(p) => format!("new “{}”", name(p)),
            Action::Linked(v) => format!("new {}", count(v.len(), "link", "links")),
        }
    }

    /// Blocking. Returns what was put back where, `(from, to)`, so windows
    /// showing a folder that moved can follow it.
    fn revert(&self) -> anyhow::Result<Vec<(PathBuf, PathBuf)>> {
        let mut moved = Vec::new();
        match self {
            Action::Renamed { from, to } => {
                anyhow::ensure!(!from.exists(), "“{}” exists again", name(from));
                std::fs::rename(to, from)?;
                moved.push((to.clone(), from.clone()));
            }
            Action::Moved(pairs) => {
                for (orig, now) in pairs {
                    let parent = orig.parent().ok_or_else(|| anyhow::anyhow!("no parent"))?;
                    std::fs::create_dir_all(parent)?;
                    let back = if orig.exists() { unique_target(parent, Path::new(&name(orig))) } else { orig.clone() };
                    if std::fs::rename(now, &back).is_ok() {
                        moved.push((now.clone(), back));
                    } else {
                        // Different filesystems: copy back, then remove.
                        let cancel = std::sync::atomic::AtomicBool::new(false);
                        noxfm_core::transfer::run(noxfm_proto::TransferOp::Move, std::slice::from_ref(now), parent, &cancel, &mut |_, _| {})?;
                        moved.push((now.clone(), parent.join(name(now))));
                    }
                }
            }
            Action::Copied(paths) | Action::Linked(paths) => {
                let present: Vec<PathBuf> = paths.iter().filter(|p| p.symlink_metadata().is_ok()).cloned().collect();
                trash::trash(&present)?;
            }
            Action::Created(p) => {
                trash::trash(std::slice::from_ref(p))?;
            }
            Action::Trashed(ids) => {
                trash::restore(ids)?;
            }
        }
        Ok(moved)
    }
}

#[derive(Default)]
pub struct Undo {
    stack: Mutex<Vec<Action>>,
}

impl Undo {
    pub fn push(&self, a: Action) {
        let mut s = self.stack.lock().unwrap();
        s.push(a);
        if s.len() > DEPTH {
            s.remove(0);
        }
    }

    pub fn label(&self) -> Option<String> {
        self.stack.lock().unwrap().last().map(Action::label)
    }

    /// Reverts the latest action. Blocking. Returns its label and what it
    /// moved back, `(from, to)`.
    pub fn undo(&self) -> anyhow::Result<(String, Vec<(PathBuf, PathBuf)>)> {
        let action = self.stack.lock().unwrap().pop().ok_or_else(|| anyhow::anyhow!("nothing to undo"))?;
        let moved = action.revert().map_err(|e| anyhow::anyhow!("couldn't undo the {}: {e}", action.label()))?;
        Ok((action.label(), moved))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        let a = Action::Renamed { from: "/d/a.txt".into(), to: "/d/b.txt".into() };
        assert_eq!(a.label(), "rename of “a.txt”");
        assert_eq!(Action::Moved(vec![("/a".into(), "/b/a".into()); 3]).label(), "move of 3 items");
        assert_eq!(Action::Trashed(vec!["x".into()]).label(), "delete of 1 item");
    }

    #[test]
    fn undoes_rename_and_move_and_keeps_depth() {
        let t = tempfile::tempdir().unwrap();
        let (a, b) = (t.path().join("a"), t.path().join("b"));
        std::fs::write(&b, "x").unwrap();
        let undo = Undo::default();
        undo.push(Action::Renamed { from: a.clone(), to: b.clone() });
        assert_eq!(undo.label().as_deref(), Some("rename of “a”"));
        let (_, moved) = undo.undo().unwrap();
        assert!(a.exists() && !b.exists());
        assert_eq!(moved, vec![(b.clone(), a.clone())]);

        let sub = t.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::rename(&a, sub.join("a")).unwrap();
        undo.push(Action::Moved(vec![(a.clone(), sub.join("a"))]));
        std::fs::write(&a, "someone made a new one").unwrap();
        let (_, moved) = undo.undo().unwrap();
        assert!(t.path().join("a (1)").exists(), "moved back beside the newcomer");
        assert_eq!(moved, vec![(sub.join("a"), t.path().join("a (1)"))]);
        assert!(undo.undo().is_err(), "stack empty");

        for i in 0..25 {
            undo.push(Action::Created(PathBuf::from(format!("/x{i}"))));
        }
        assert_eq!(undo.stack.lock().unwrap().len(), DEPTH);
    }
}
