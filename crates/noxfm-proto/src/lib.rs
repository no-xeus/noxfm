//! Wire protocol between `noxd` and its child processes.
//!
//! Frames are `u32` little-endian length + postcard payload. Every connection
//! starts with a [`Hello`] from the client answered by a [`Welcome`].

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

mod client;
mod framing;
pub use client::{Client, ClientError};
pub use framing::{FrameError, read_frame, write_frame};

/// Bumped on any incompatible change to the types below.
pub const PROTOCOL_VERSION: u32 = 1;

/// Hash of this crate's source, so mismatched dev builds fail the handshake
/// even when nobody remembered to bump [`PROTOCOL_VERSION`].
pub const SCHEMA: &str = env!("NOXFM_SCHEMA");

/// The release, from the workspace `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The exact source revision this was built from (`git describe`).
pub const GIT_REVISION: &str = env!("NOXFM_GIT");

/// `noxd 0.1.0 (v0.1.0-3-g1a2b3c4)`
pub fn version_line(program: &str) -> String {
    format!("{program} {VERSION} ({GIT_REVISION})")
}

/// Env var the daemon sets on spawned children.
pub const SOCKET_ENV: &str = "NOXFM_SOCKET";

/// `$XDG_RUNTIME_DIR/noxfm/noxd.sock`, overridable via [`SOCKET_ENV`].
pub fn socket_path() -> PathBuf {
    if let Some(p) = std::env::var_os(SOCKET_ENV) {
        return PathBuf::from(p);
    }
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("noxfm").join("noxd.sock")
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Hello {
    pub version: u32,
    pub schema: String,
    pub role: Role,
    /// The client's display variables. noxd may have been started before the
    /// graphical session (by systemd); windows it opens use the latest of these.
    pub display_env: Vec<(String, String)>,
}

/// Environment variables a window needs to reach the user's display.
pub const DISPLAY_VARS: &[&str] =
    &["WAYLAND_DISPLAY", "DISPLAY", "XDG_CURRENT_DESKTOP", "XDG_SESSION_TYPE", "XDG_SESSION_DESKTOP", "HYPRLAND_INSTANCE_SIGNATURE"];

impl Hello {
    pub fn new(role: Role) -> Self {
        let display_env = DISPLAY_VARS
            .iter()
            .filter_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()).map(|v| (k.to_string(), v)))
            .collect();
        Hello { version: PROTOCOL_VERSION, schema: SCHEMA.into(), role, display_env }
    }

    pub fn compatible(&self) -> bool {
        self.version == PROTOCOL_VERSION && self.schema == SCHEMA
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum Role {
    Launcher,
    Browser,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Welcome {
    Ok { daemon_version: u32 },
    VersionMismatch { daemon_version: u32 },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClientMsg {
    pub id: u64,
    pub req: Request,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ServerMsg {
    Reply { id: u64, result: Result<Response, String> },
    Event(Event),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Request {
    /// With `watch`, the caller is also subscribed to `path` (see `Subscribe`)
    /// before any background work for the listing starts.
    ListDir { path: PathBuf, watch: bool },
    Stat { path: PathBuf },
    Complete { prefix: String },
    Subscribe { path: PathBuf },
    Unsubscribe { path: PathBuf },
    Transfer { op: TransferOp, sources: Vec<PathBuf>, dest: PathBuf },
    CancelTransfer { id: u64 },
    ListTransfers,
    /// A new browser window at `path`, or showing `view` (Recent, Trash).
    /// `layout` is the opening window's, which the new one starts with.
    OpenWindow { path: Option<PathBuf>, view: Option<StartView>, layout: Option<WindowLayout> },
    OpenWith { path: PathBuf, app: Option<String> },
    Mount { device: String },
    Unmount { device: String },
    SetMountPolicy { uuid: String, policy: MountPolicy },
    /// Answers a pending "mount this?" question without mounting.
    DismissAsk { device: String },
    ListDevices,
    Places,
    Recent { kind: Option<RecentKind>, limit: u32 },
    ForgetRecent { path: PathBuf },
    Thumbnail { path: PathBuf },

    // File operations (all undoable except DeleteForever).
    Rename { path: PathBuf, new_name: String },
    /// Replies `Response::Path` with what was created.
    Create { dir: PathBuf, kind: NewKind },
    Symlink { targets: Vec<PathBuf>, dir: PathBuf },
    Trash { paths: Vec<PathBuf> },
    DeleteForever { paths: Vec<PathBuf> },
    /// Into a new `<name>.zip` (made unique) in `dest_dir`.
    Compress { sources: Vec<PathBuf>, dest_dir: PathBuf },
    Extract { zip: PathBuf, dest_dir: PathBuf },
    Chmod { path: PathBuf, mode: u32 },
    Undo,

    // Trash.
    ListTrash,
    RestoreTrash { ids: Vec<String> },
    PurgeTrash { ids: Vec<String> },
    EmptyTrash,

    Properties { paths: Vec<PathBuf> },
    AppsFor { mime: String },
    AllApps,
    SetDefaultApp { mime: String, app: String },
    OpenTerminal { dir: PathBuf },
    Pin { path: PathBuf },
    Unpin { path: PathBuf },
    /// Sidebar sections and disks the user folded: `section:<name>`, `disk:<drive id>`.
    SidebarCollapsed,
    SetSidebarCollapsed { key: String, collapsed: bool },
    /// Current undo label, also pushed as `Event::UndoChanged`.
    UndoLabel,
    /// What's installed / available, and which features suffer if not.
    Health,
    /// Stop showing the "missing features" banner (until noxd restarts).
    DismissHealth,
}

/// Window layout a new window inherits from the one that opened it.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct WindowLayout {
    pub sidebar_open: bool,
    pub sidebar_width: f32,
}

impl WindowLayout {
    /// `--sidebar=240` or `--sidebar=hidden:240`
    pub fn to_arg(self) -> String {
        let w = self.sidebar_width.round() as u32;
        if self.sidebar_open { format!("--sidebar={w}") } else { format!("--sidebar=hidden:{w}") }
    }

    pub fn from_arg(arg: &str) -> Option<WindowLayout> {
        let v = arg.strip_prefix("--sidebar=")?;
        let (open, w) = match v.strip_prefix("hidden:") {
            Some(w) => (false, w),
            None => (true, v),
        };
        Some(WindowLayout { sidebar_open: open, sidebar_width: w.parse().ok()? })
    }
}

/// A window that opens on something other than a folder.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum StartView {
    Recent(Option<RecentKind>),
    Trash,
}

impl StartView {
    /// `--recent`, `--recent=downloaded`, `--trash`
    pub fn to_arg(self) -> String {
        match self {
            StartView::Trash => "--trash".into(),
            StartView::Recent(None) => "--recent".into(),
            StartView::Recent(Some(RecentKind::Downloaded)) => "--recent=downloaded".into(),
            StartView::Recent(Some(RecentKind::Modified)) => "--recent=edited".into(),
            StartView::Recent(Some(RecentKind::Created)) => "--recent=created".into(),
        }
    }

    pub fn from_arg(arg: &str) -> Option<StartView> {
        Some(match arg {
            "--trash" => StartView::Trash,
            "--recent" => StartView::Recent(None),
            "--recent=downloaded" => StartView::Recent(Some(RecentKind::Downloaded)),
            "--recent=edited" => StartView::Recent(Some(RecentKind::Modified)),
            "--recent=created" => StartView::Recent(Some(RecentKind::Created)),
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum NewKind {
    Folder,
    EmptyFile,
    TextDocument,
    /// Copy of a file from ~/Templates.
    Template(PathBuf),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Response {
    Ok,
    /// `fs` is the filesystem name (`btrfs`, `ext4`, `ntfs`, …).
    Dir { path: PathBuf, fs: Option<String>, entries: Vec<Entry> },
    Entry(Entry),
    Completions(Vec<String>),
    TransferStarted { id: u64 },
    Transfers(Vec<TransferStatus>),
    Devices(Vec<Device>),
    /// Where a device got mounted.
    Mounted(PathBuf),
    Places(Vec<Place>),
    Recent(Vec<(RecentItem, Entry)>),
    Thumbnail { cache_path: Option<PathBuf> },
    Path(PathBuf),
    /// Each item with an `Entry` of the trashed file under its original name.
    TrashItems(Vec<(TrashEntry, Entry)>),
    Properties(Box<Props>),
    Apps(Vec<AppRef>),
    /// What was undone ("Rename “a” to “b”"), or the pending undo label.
    Label(Option<String>),
    Keys(Vec<String>),
    Health { checks: Vec<HealthCheck>, dismissed: bool },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HealthCheck {
    /// What was checked: "ffmpeg", "UDisks2", …
    pub name: String,
    pub ok: bool,
    /// What was found, or how to fix it.
    pub detail: String,
    /// The feature affected when not ok.
    pub feature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TrashEntry {
    pub id: String,
    pub name: String,
    pub original_path: PathBuf,
    pub deleted_at: Timestamp,
    /// When the automatic purge will delete it for good.
    pub purge_at: Option<Timestamp>,
    pub size: Option<u64>,
    pub is_dir: bool,
}

/// Everything the Properties panel shows, for one or more paths.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Props {
    pub paths: Vec<PathBuf>,
    /// Set when exactly one path was asked for.
    pub entry: Option<Entry>,
    pub accessed: Option<Timestamp>,
    pub files: u64,
    pub folders: u64,
    pub bytes: u64,
    /// Some subfolders couldn't be read; totals are a lower bound.
    pub partial: bool,
    pub fs: Option<String>,
    /// Detected from content; `ext_mime` is what the extension claims.
    pub ext_mime: Option<String>,
    pub git: Option<GitInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GitInfo {
    pub branch: Option<String>,
    pub dirty: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Event {
    DirChanged { path: PathBuf },
    SizeUpdated { path: PathBuf, bytes: u64 },
    ThumbnailReady { path: PathBuf, cache_path: PathBuf },
    TransferProgress(TransferStatus),
    TransferDone { id: u64, error: Option<String> },
    /// Devices appeared, disappeared, got (un)mounted or need an answer.
    DevicesChanged,
    RecentChanged,
    TrashChanged { items: u32 },
    UndoChanged(Option<String>),
    PlacesChanged,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    /// Dangling symlink; working links report their target's kind.
    BrokenLink,
    Other,
}

/// Seconds since the Unix epoch.
pub type Timestamp = i64;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub kind: EntryKind,
    pub symlink: bool,
    /// File size, or recursive size for directories once known.
    pub size: Option<u64>,
    pub created: Option<Timestamp>,
    pub modified: Option<Timestamp>,
    pub uid: u32,
    pub gid: u32,
    pub owner: Option<String>,
    pub group: Option<String>,
    pub mode: u32,
    pub mime: Option<String>,
    /// Extension says one type, magic number says another.
    pub mime_mismatch: bool,
    pub is_git: bool,
    pub hidden: bool,
    /// Application that opens this entry by default (FastOpen).
    pub app: Option<AppRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AppRef {
    /// Desktop file ID.
    pub id: String,
    pub name: String,
    /// Icon theme name or absolute path.
    pub icon: Option<String>,
}

impl Entry {
    pub fn extension(&self) -> Option<&str> {
        if self.kind == EntryKind::Dir {
            return None;
        }
        Path::new(&self.name).extension().and_then(|e| e.to_str())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum JobKind {
    Copy,
    Move,
    Compress,
    Extract,
}

impl From<TransferOp> for JobKind {
    fn from(op: TransferOp) -> Self {
        match op {
            TransferOp::Copy => JobKind::Copy,
            TransferOp::Move => JobKind::Move,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum TransferOp {
    Copy,
    Move,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TransferStatus {
    pub id: u64,
    pub kind: JobKind,
    pub items: u32,
    /// Still adding up sizes; `total_bytes` isn't final yet.
    pub counting: bool,
    pub done_bytes: u64,
    pub total_bytes: u64,
    /// Time spent copying so far (excludes counting), measured by the daemon.
    pub elapsed_ms: u64,
    pub current: Option<PathBuf>,
    pub dest: PathBuf,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum MountPolicy {
    Auto,
    #[default]
    Ask,
    Never,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Device {
    /// udisks object path.
    pub id: String,
    /// `/dev/sdb1`
    pub device: String,
    pub label: Option<String>,
    pub uuid: Option<String>,
    pub fs_type: Option<String>,
    pub size: u64,
    pub mount_point: Option<PathBuf>,
    /// Drive model, e.g. "Samsung SSD 870 QVO 2TB".
    pub drive: Option<String>,
    /// udisks object path of the physical disk; partitions of one disk share it.
    /// `None` for disk images and other drive-less devices.
    pub drive_id: Option<String>,
    pub drive_size: u64,
    /// Partition number on its disk (`sda1` -> 1); `None` for unpartitioned media.
    pub partition: Option<u32>,
    /// Built into the machine rather than plugged in.
    pub internal: bool,
    pub policy: MountPolicy,
    /// Just plugged in with policy "ask": waiting for the user.
    pub asking: bool,
    /// Free bytes, when mounted.
    pub free: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Place {
    pub name: String,
    pub path: PathBuf,
    /// Added by the user ("Pin to sidebar"), rather than a standard folder.
    pub pinned: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum RecentKind {
    Created,
    Modified,
    Downloaded,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecentItem {
    pub path: PathBuf,
    pub kind: RecentKind,
    pub at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_args_round_trip() {
        for l in [WindowLayout { sidebar_open: true, sidebar_width: 300.0 }, WindowLayout { sidebar_open: false, sidebar_width: 180.0 }] {
            assert_eq!(WindowLayout::from_arg(&l.to_arg()), Some(l));
        }
        assert_eq!(WindowLayout::from_arg("--sidebar=x"), None);
        for v in [StartView::Trash, StartView::Recent(None), StartView::Recent(Some(RecentKind::Created))] {
            assert_eq!(StartView::from_arg(&v.to_arg()), Some(v));
        }
    }
}
