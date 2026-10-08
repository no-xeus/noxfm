//! Trash, undo and other file operations through the daemon.
//!
//! One test, in its own process: it points XDG_DATA_HOME at a temp folder so
//! the user's real trash is never touched (env vars are process-wide).

use std::path::Path;
use std::time::Duration;

use noxfm_proto::{Client, NewKind, Request, Response, Role};
use tokio::net::UnixListener;

async fn ask(c: &Client, r: Request) -> Response {
    tokio::time::timeout(Duration::from_secs(10), c.request(r)).await.expect("timeout").expect("request failed")
}

#[tokio::test]
async fn trash_undo_and_friends() {
    let tmp = tempfile::tempdir().unwrap();
    // SAFETY: set before any other thread of this test process reads the env.
    unsafe {
        std::env::set_var("XDG_DATA_HOME", tmp.path().join("data"));
        std::env::set_var("HOME", tmp.path());
    }
    let sock = tmp.path().join("noxd.sock");
    tokio::spawn(noxd::Daemon::headless(sock.clone()).serve(UnixListener::bind(&sock).unwrap()));
    let (c, _events) = Client::connect(&sock, Role::Browser).await.unwrap();
    let work = tmp.path().join("work");
    std::fs::create_dir(&work).unwrap();

    // New folder, rename it, undo the rename, undo the creation.
    let Response::Path(folder) = ask(&c, Request::Create { dir: work.clone(), kind: NewKind::Folder }).await else { panic!() };
    assert_eq!(folder, work.join("New folder"));
    let Response::Path(renamed) = ask(&c, Request::Rename { path: folder.clone(), new_name: "Stuff".into() }).await else { panic!() };
    assert_eq!(ask(&c, Request::UndoLabel).await, Response::Label(Some("rename of “New folder”".into())));
    ask(&c, Request::Undo).await;
    assert!(folder.exists() && !renamed.exists());
    ask(&c, Request::Undo).await;
    assert!(!folder.exists(), "undoing a creation trashes it");

    // Trash a file, see it listed, undo restores it.
    let f = work.join("report.txt");
    std::fs::write(&f, "data").unwrap();
    ask(&c, Request::Trash { paths: vec![f.clone()] }).await;
    assert!(!f.exists());
    let Response::TrashItems(items) = ask(&c, Request::ListTrash).await else { panic!() };
    let (item, entry) = items.iter().find(|(i, _)| i.original_path == f).expect("in trash");
    assert_eq!(entry.name, "report.txt");
    assert_eq!(item.size, Some(4));
    ask(&c, Request::Undo).await;
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "data");

    // Restore onto a taken name keeps both.
    ask(&c, Request::Trash { paths: vec![f.clone()] }).await;
    std::fs::write(&f, "newer").unwrap();
    let Response::TrashItems(items) = ask(&c, Request::ListTrash).await else { panic!() };
    let id = items.iter().find(|(i, _)| i.original_path == f).unwrap().0.id.clone();
    ask(&c, Request::RestoreTrash { ids: vec![id] }).await;
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "newer");
    assert_eq!(std::fs::read_to_string(work.join("report (1).txt")).unwrap(), "data");

    // The 30-day rule: only old items go.
    std::fs::write(work.join("old"), "").unwrap();
    ask(&c, Request::Trash { paths: vec![work.join("old"), work.join("report (1).txt")] }).await;
    assert_eq!(noxd::trash::purge_older_than(30, noxd::trash::now()).unwrap(), 0);
    // Also the "New folder" that undoing its creation put in the trash.
    assert_eq!(noxd::trash::purge_older_than(30, noxd::trash::now() + 31 * 86_400).unwrap(), 3);
    let Response::TrashItems(left) = ask(&c, Request::ListTrash).await else { panic!() };
    assert!(left.iter().all(|(i, _)| !i.original_path.starts_with(&work)), "{left:?}");

    // Links, chmod, properties, permanent delete.
    ask(&c, Request::Symlink { targets: vec![f.clone()], dir: work.clone() }).await;
    assert_eq!(std::fs::read_link(work.join("report.txt (link)")).unwrap(), f);
    ask(&c, Request::Chmod { path: f.clone(), mode: 0o600 }).await;
    let Response::Properties(p) = ask(&c, Request::Properties { paths: vec![f.clone()] }).await else { panic!() };
    assert_eq!(p.entry.unwrap().mode & 0o777, 0o600);
    ask(&c, Request::DeleteForever { paths: vec![work.join("report.txt (link)")] }).await;
    assert!(!Path::new(&work.join("report.txt (link)")).exists());

    // Compress + extract run as jobs.
    let zip = work.join("report.zip");
    let Response::TransferStarted { .. } = ask(&c, Request::Compress { sources: vec![f.clone()], dest_dir: work.clone() }).await else { panic!() };
    for _ in 0..100 {
        if zip.exists() && ask(&c, Request::ListTransfers).await == Response::Transfers(vec![]) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    ask(&c, Request::Extract { zip: zip.clone(), dest_dir: work.clone() }).await;
    for _ in 0..100 {
        if work.join("report/report.txt").exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(std::fs::read_to_string(work.join("report/report.txt")).unwrap(), "newer");
}
