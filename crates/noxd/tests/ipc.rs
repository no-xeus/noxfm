use std::time::Duration;

use noxfm_proto::{Client, ClientError, Event, Request, Response, Role, TransferOp};
use tokio::net::UnixListener;

async fn start() -> (tempfile::TempDir, Client) {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("noxd.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    tokio::spawn(noxd::Daemon::headless(sock.clone()).serve(listener));
    let (client, _events) = Client::connect(&sock, Role::Browser).await.unwrap();
    (tmp, client)
}

#[tokio::test]
async fn list_dir_and_complete() {
    let (tmp, client) = start().await;
    let dir = tmp.path().join("data");
    std::fs::create_dir_all(dir.join("Sub")).unwrap();
    std::fs::write(dir.join("x.bin"), [0u8; 10]).unwrap();

    let resp = client.request(Request::ListDir { path: dir.clone(), watch: false }).await.unwrap();
    let Response::Dir { entries, .. } = resp else { panic!("{resp:?}") };
    let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["Sub", "x.bin"]);

    let prefix = format!("{}/S", dir.display());
    let resp = client.request(Request::Complete { prefix }).await.unwrap();
    assert_eq!(resp, Response::Completions(vec![format!("{}/Sub/", dir.display())]));
}

#[tokio::test]
async fn errors_are_reported_not_fatal() {
    let (_tmp, client) = start().await;
    let err = client.request(Request::ListDir { path: "/definitely/not/here".into(), watch: false }).await;
    assert!(matches!(err, Err(ClientError::Remote(_))), "{err:?}");
    // Connection still usable afterwards.
    let ok = tokio::time::timeout(Duration::from_secs(5), client.request(Request::ListDir { path: "/".into(), watch: false }))
        .await
        .unwrap();
    assert!(ok.is_ok());
}

#[tokio::test]
async fn watch_reports_changes_and_sizes() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("noxd.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    tokio::spawn(noxd::Daemon::headless(sock.clone()).serve(listener));
    let (client, mut events) = Client::connect(&sock, Role::Browser).await.unwrap();

    let dir = tmp.path().join("data");
    std::fs::create_dir_all(dir.join("sub/deep")).unwrap();
    std::fs::write(dir.join("sub/a"), [0u8; 1000]).unwrap();
    std::fs::write(dir.join("sub/deep/b"), [0u8; 234]).unwrap();

    client.request(Request::ListDir { path: dir.clone(), watch: true }).await.unwrap();

    assert_eq!(next(&mut events).await, Event::SizeUpdated { path: dir.join("sub"), bytes: 1234 });

    std::fs::write(dir.join("new.txt"), "x").unwrap();
    assert_eq!(next(&mut events).await, Event::DirChanged { path: dir.clone() });

    // Re-listing serves the cached size without walking again.
    let Response::Dir { entries, .. } =
        client.request(Request::ListDir { path: dir.clone(), watch: true }).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(entries.iter().find(|e| e.name == "sub").unwrap().size, Some(1234));
}

async fn next(events: &mut tokio::sync::mpsc::UnboundedReceiver<Event>) -> Event {
    tokio::time::timeout(Duration::from_secs(5), events.recv()).await.expect("event timeout").unwrap()
}

#[tokio::test]
async fn transfer_reports_progress_and_completion() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("noxd.sock");
    tokio::spawn(noxd::Daemon::headless(sock.clone()).serve(UnixListener::bind(&sock).unwrap()));
    let (client, mut events) = Client::connect(&sock, Role::Browser).await.unwrap();

    let src = tmp.path().join("src.bin");
    std::fs::write(&src, vec![9u8; 5 << 20]).unwrap();
    let dest = tmp.path().join("out");
    std::fs::create_dir(&dest).unwrap();

    let req = Request::Transfer { op: TransferOp::Copy, sources: vec![src.clone()], dest: dest.clone() };
    let Response::TransferStarted { id } = client.request(req).await.unwrap() else { panic!() };

    let mut saw_total = false;
    loop {
        match next(&mut events).await {
            Event::TransferProgress(s) => {
                assert_eq!(s.id, id);
                saw_total |= !s.counting && s.total_bytes == 5 << 20;
            }
            Event::TransferDone { id: done, error } => {
                assert_eq!((done, error), (id, None));
                break;
            }
            Event::UndoChanged(label) => assert!(label.is_some_and(|l| l.contains("src.bin"))),
            other => panic!("unexpected {other:?}"),
        }
    }
    assert!(saw_total);
    assert_eq!(std::fs::metadata(dest.join("src.bin")).unwrap().len(), 5 << 20);
    let Response::Transfers(left) = client.request(Request::ListTransfers).await.unwrap() else { panic!() };
    assert!(left.is_empty());

    // Failures are reported, not swallowed.
    let req = Request::Transfer { op: TransferOp::Copy, sources: vec![tmp.path().join("nope")], dest };
    client.request(req).await.unwrap();
    loop {
        if let Event::TransferDone { error, .. } = next(&mut events).await {
            assert!(error.unwrap().contains("nope"));
            break;
        }
    }
}

#[tokio::test]
async fn rename_and_its_undo_report_the_move() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("noxd.sock");
    tokio::spawn(noxd::Daemon::headless(sock.clone()).serve(UnixListener::bind(&sock).unwrap()));
    let (client, mut events) = Client::connect(&sock, Role::Browser).await.unwrap();
    let (old, new) = (tmp.path().join("old"), tmp.path().join("new"));
    std::fs::create_dir(&old).unwrap();

    client.request(Request::Rename { path: old.clone(), new_name: "new".into() }).await.unwrap();
    assert_eq!(moved(&mut events).await, vec![(old.clone(), new.clone())]);

    // Windows inside "new" follow it back.
    client.request(Request::Undo).await.unwrap();
    assert_eq!(moved(&mut events).await, vec![(new, old.clone())]);
    assert!(old.is_dir());
}

async fn moved(events: &mut tokio::sync::mpsc::UnboundedReceiver<Event>) -> Vec<(std::path::PathBuf, std::path::PathBuf)> {
    loop {
        if let Event::Moved(m) = next(events).await {
            return m;
        }
    }
}
