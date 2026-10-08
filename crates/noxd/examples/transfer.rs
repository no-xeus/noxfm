//! Dev helper: `cargo run -p noxd --example transfer -- copy|move SRC... DEST`
//! or `-- open FILE` (FastOpen), `-- devices|places|recent`, `-- mount|unmount ID`, `-- trash PATH...`.
use noxfm_proto::{Client, Request, Role, TransferOp};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let (client, _) = Client::connect(&noxfm_proto::socket_path(), Role::Launcher).await?;
    if args[0] == "devices" || args[0] == "places" || args[0] == "recent" {
        let req = match args[0].as_str() {
            "devices" => Request::ListDevices,
            "places" => Request::Places,
            _ => Request::Recent { kind: None, limit: 10 },
        };
        match client.request(req).await? {
            noxfm_proto::Response::Devices(ds) => ds.iter().for_each(|d| println!("{d:?}")),
            noxfm_proto::Response::Places(ps) => ps.iter().for_each(|p| println!("{} {}", p.name, p.path.display())),
            noxfm_proto::Response::Recent(rs) => rs.iter().for_each(|(r, _)| println!("{:?} {}", r.kind, r.path.display())),
            other => println!("{other:?}"),
        }
        return Ok(());
    }
    if args[0] == "mount" || args[0] == "unmount" {
        let device = args[1].clone();
        let req = if args[0] == "mount" { Request::Mount { device } } else { Request::Unmount { device } };
        println!("{:?}", client.request(req).await?);
        return Ok(());
    }
    if args[0] == "trash" {
        let paths = args[1..].iter().map(Into::into).collect();
        println!("{:?}", client.request(Request::Trash { paths }).await?);
        return Ok(());
    }
    if args[0] == "open" {
        let req = Request::OpenWith { path: args[1].clone().into(), app: None };
        println!("{:?}", client.request(req).await?);
        return Ok(());
    }
    let op = match args.remove(0).as_str() {
        "move" => TransferOp::Move,
        _ => TransferOp::Copy,
    };
    let dest = args.pop().unwrap().into();
    let sources = args.into_iter().map(Into::into).collect();
    println!("{:?}", client.request(Request::Transfer { op, sources, dest }).await?);
    Ok(())
}
