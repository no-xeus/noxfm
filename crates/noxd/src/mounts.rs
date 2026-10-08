//! Drives and partitions through UDisks2 (system D-Bus), with per-filesystem
//! policies: mount automatically, ask, or never.
//!
//! - At startup, devices whose policy is "auto" get mounted.
//! - A device that appears later gets its policy applied: "ask" (the default
//!   for unknown devices) shows a question in the browser windows.
//! - Mounting goes through UDisks, so polkit decides who may mount what.

use std::collections::{HashMap, HashSet};
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use noxfm_proto::{Device, Event, MountPolicy, Role};
use zbus::Connection;
use zbus::fdo::ManagedObjects;
use zbus::zvariant::{OwnedValue, Value};

use crate::Daemon;

const UDISKS: &str = "org.freedesktop.UDisks2";
const BLOCK: &str = "org.freedesktop.UDisks2.Block";
const FILESYSTEM: &str = "org.freedesktop.UDisks2.Filesystem";
const DRIVE: &str = "org.freedesktop.UDisks2.Drive";
const LOOP: &str = "org.freedesktop.UDisks2.Loop";
const PARTITION: &str = "org.freedesktop.UDisks2.Partition";
/// Bursts of UDisks signals (a plug-in emits many) are handled once.
const SETTLE: Duration = Duration::from_millis(400);

/// What we read about one block device, before deciding whether to show it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RawBlock {
    pub id: String,
    pub device: String,
    pub label: String,
    pub uuid: String,
    pub fs_type: String,
    pub usage: String,
    pub size: u64,
    pub hint_ignore: bool,
    pub hint_system: bool,
    pub hint_name: String,
    pub has_filesystem: bool,
    pub mount_points: Vec<PathBuf>,
    pub drive_model: String,
    pub drive_id: String,
    pub drive_size: u64,
    pub partition: Option<u32>,
    pub drive_removable: bool,
    /// A disk image a user attached (`udisksctl loop-setup`, opening an ISO):
    /// treated like a plugged-in drive.
    pub user_loop: bool,
}

/// Where user-facing mounts live; anything else (`/`, `/boot`, `/home`) is
/// the system's business and not shown.
fn user_mount(p: &Path) -> bool {
    ["/run/media", "/media", "/mnt"].iter().any(|root| p.starts_with(root))
}

/// The devices worth showing, labelled and with their policy.
pub fn to_devices(raw: &[RawBlock], policies: &HashMap<String, MountPolicy>, asking: &HashSet<String>) -> Vec<Device> {
    let mut out: Vec<Device> = raw
        .iter()
        .filter(|b| b.has_filesystem && b.usage == "filesystem" && !b.hint_ignore)
        .filter(|b| b.mount_points.iter().all(|m| user_mount(m)))
        .map(|b| {
            let opt = |s: &str| (!s.is_empty()).then(|| s.to_owned());
            let internal = b.hint_system && !b.drive_removable && !b.user_loop;
            Device {
                id: b.id.clone(),
                device: b.device.clone(),
                label: opt(&b.label).or_else(|| opt(&b.hint_name)),
                uuid: opt(&b.uuid),
                fs_type: opt(&b.fs_type),
                size: b.size,
                mount_point: b.mount_points.first().cloned(),
                drive: opt(&b.drive_model),
                drive_id: opt(&b.drive_id),
                drive_size: b.drive_size,
                partition: b.partition,
                internal,
                policy: policies.get(&b.uuid).copied().unwrap_or(if internal { MountPolicy::Never } else { MountPolicy::Ask }),
                asking: asking.contains(&b.id),
                free: None,
            }
        })
        .collect();
    // Plugged-in devices first, then by disk and partition number.
    out.sort_by(|a, b| {
        a.internal
            .cmp(&b.internal)
            .then_with(|| a.drive_id.cmp(&b.drive_id))
            .then_with(|| a.partition.cmp(&b.partition))
            .then_with(|| a.device.cmp(&b.device))
    });
    out
}

fn read_blocks(objs: &ManagedObjects) -> Vec<RawBlock> {
    let s = |props: &HashMap<String, OwnedValue>, key: &str| -> String {
        props.get(key).and_then(|v| v.try_clone().ok()).and_then(|v| String::try_from(v).ok()).unwrap_or_default()
    };
    let b = |props: &HashMap<String, OwnedValue>, key: &str| -> bool {
        props.get(key).and_then(|v| bool::try_from(v).ok()).unwrap_or(false)
    };
    // `ay` values are NUL-terminated byte strings.
    let bytes_path = |v: Vec<u8>| PathBuf::from(std::ffi::OsString::from_vec(v.into_iter().take_while(|&c| c != 0).collect()));

    objs.iter()
        .filter_map(|(path, ifaces)| {
            let block = ifaces.iter().find(|(n, _)| n.as_str() == BLOCK)?.1;
            let fs = ifaces.iter().find(|(n, _)| n.as_str() == FILESYSTEM).map(|(_, p)| p);
            let drive_path = block
                .get("Drive")
                .and_then(|v| v.try_clone().ok())
                .and_then(|v| zbus::zvariant::OwnedObjectPath::try_from(v).ok())
                .filter(|p| p.as_str() != "/");
            let drive = drive_path.as_ref().and_then(|dp| {
                objs.iter().find(|(p, _)| p.as_str() == dp.as_str())?.1.iter().find(|(n, _)| n.as_str() == DRIVE).map(|(_, p)| p)
            });
            let device = block
                .get("PreferredDevice")
                .and_then(|v| v.try_clone().ok())
                .and_then(|v| Vec::<u8>::try_from(v).ok())
                .map(|v| bytes_path(v).display().to_string())
                .unwrap_or_default();
            let mount_points = fs
                .and_then(|p| p.get("MountPoints"))
                .and_then(|v| v.try_clone().ok())
                .and_then(|v| Vec::<Vec<u8>>::try_from(v).ok())
                .map(|mps| mps.into_iter().map(bytes_path).collect())
                .unwrap_or_default();
            Some(RawBlock {
                id: path.to_string(),
                device,
                label: s(block, "IdLabel"),
                uuid: s(block, "IdUUID"),
                fs_type: s(block, "IdType"),
                usage: s(block, "IdUsage"),
                size: block.get("Size").and_then(|v| u64::try_from(v).ok()).unwrap_or(0),
                hint_ignore: b(block, "HintIgnore"),
                hint_system: b(block, "HintSystem"),
                hint_name: s(block, "HintName"),
                has_filesystem: fs.is_some(),
                mount_points,
                drive_model: drive.map(|d| s(d, "Model")).unwrap_or_default(),
                drive_id: drive.and(drive_path.as_ref()).map(|p| p.to_string()).unwrap_or_default(),
                drive_size: drive.and_then(|d| d.get("Size")).and_then(|v| u64::try_from(v).ok()).unwrap_or(0),
                partition: ifaces
                    .iter()
                    .find(|(n, _)| n.as_str() == PARTITION)
                    .and_then(|(_, p)| p.get("Number"))
                    .and_then(|v| u32::try_from(v).ok()),
                drive_removable: drive.is_some_and(|d| b(d, "Removable") || b(d, "MediaRemovable") || b(d, "Ejectable")),
                user_loop: ifaces
                    .iter()
                    .find(|(n, _)| n.as_str() == LOOP)
                    .and_then(|(_, p)| p.get("SetupByUID"))
                    .and_then(|v| u32::try_from(v).ok())
                    .is_some_and(|uid| uid != 0),
            })
        })
        .collect()
}

#[derive(Default)]
struct State {
    conn: Option<Connection>,
    raw: Vec<RawBlock>,
    /// Devices already shown: any other that becomes showable was "plugged in".
    known: HashSet<String>,
    asking: HashSet<String>,
    policies: HashMap<String, MountPolicy>,
}

pub struct Mounts {
    state: Mutex<State>,
    policy_file: PathBuf,
    enabled: bool,
}

impl Mounts {
    pub fn for_user() -> Self {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| noxfm_core::complete::home_dir().join(".config"));
        let policy_file = config.join("noxfm/mounts.conf");
        let policies = std::fs::read_to_string(&policy_file).map(|t| noxfm_core::places::parse_policies(&t)).unwrap_or_default();
        Mounts { state: Mutex::new(State { policies, ..Default::default() }), policy_file, enabled: true }
    }

    pub fn connected(&self) -> bool {
        self.state.lock().unwrap().conn.is_some()
    }

    pub fn disabled() -> Self {
        Mounts { state: Default::default(), policy_file: PathBuf::new(), enabled: false }
    }

    pub fn devices(&self) -> Vec<Device> {
        let mut devices = {
            let s = self.state.lock().unwrap();
            to_devices(&s.raw, &s.policies, &s.asking)
        };
        for d in &mut devices {
            d.free = d.mount_point.as_deref().and_then(|m| rustix::fs::statvfs(m).ok()).map(|v| v.f_bavail * v.f_frsize);
        }
        devices
    }

    fn conn(&self) -> anyhow::Result<Connection> {
        self.state.lock().unwrap().conn.clone().ok_or_else(|| anyhow::anyhow!("UDisks2 is not available"))
    }

    fn save_policies(&self) {
        let text = noxfm_core::places::format_policies(&self.state.lock().unwrap().policies);
        let write = || -> std::io::Result<()> {
            std::fs::create_dir_all(self.policy_file.parent().expect("has parent"))?;
            std::fs::write(&self.policy_file, text)
        };
        if let Err(e) = write() {
            tracing::warn!(%e, "could not save mount policies");
        }
    }
}

impl Daemon {
    pub fn start_mounts(self: &Arc<Self>) {
        if !self.mounts.enabled {
            return;
        }
        let me = self.clone();
        tokio::spawn(async move {
            if let Err(e) = me.run_mounts().await {
                tracing::warn!(%e, "mount manager stopped");
            }
        });
    }

    async fn run_mounts(self: &Arc<Self>) -> anyhow::Result<()> {
        let conn = Connection::system().await?;
        self.mounts.state.lock().unwrap().conn = Some(conn.clone());
        let rule = zbus::MatchRule::builder().msg_type(zbus::message::Type::Signal).sender(UDISKS)?.build();
        let mut signals = zbus::MessageStream::for_match_rule(rule, &conn, None).await?;

        let added = self.rescan().await?;
        // At startup everything is "already there": only auto-mount.
        for id in added {
            let policy = self.devices_by_id(&id).map(|d| d.policy);
            if policy == Some(MountPolicy::Auto) {
                let _ = self.mount(&id).await;
            }
        }
        tracing::info!(devices = self.mounts.devices().len(), "mounts: watching UDisks2");

        while signals.next().await.is_some() {
            // Let the burst settle, then take one look.
            let _ = tokio::time::timeout(SETTLE, async { while signals.next().await.is_some() {} }).await;
            let added = self.rescan().await?;
            for id in added {
                self.plugged_in(&id).await;
            }
            self.hub.broadcast(Event::DevicesChanged);
        }
        Ok(())
    }

    fn devices_by_id(&self, id: &str) -> Option<Device> {
        self.mounts.devices().into_iter().find(|d| d.id == id)
    }

    /// Re-reads UDisks; returns devices (shown ones) not seen before.
    async fn rescan(&self) -> anyhow::Result<Vec<String>> {
        let conn = self.mounts.conn()?;
        let om = zbus::fdo::ObjectManagerProxy::builder(&conn)
            .destination(UDISKS)?
            .path("/org/freedesktop/UDisks2")?
            .build()
            .await?;
        let raw = read_blocks(&om.get_managed_objects().await?);
        let mut s = self.mounts.state.lock().unwrap();
        s.raw = raw;
        let shown: Vec<String> = to_devices(&s.raw, &s.policies, &s.asking).into_iter().map(|d| d.id).collect();
        let added: Vec<String> = shown.iter().filter(|id| !s.known.contains(*id)).cloned().collect();
        // Only devices that were showable count as seen: UDisks announces a
        // block device first and its filesystem a moment later.
        let present: HashSet<String> = s.raw.iter().map(|b| b.id.clone()).collect();
        s.known.retain(|id| present.contains(id));
        s.known.extend(shown);
        s.asking.retain(|id| present.contains(id));
        Ok(added)
    }

    async fn plugged_in(&self, id: &str) {
        let Some(dev) = self.devices_by_id(id) else { return };
        tracing::info!(device = %dev.device, policy = ?dev.policy, "device appeared");
        match dev.policy {
            MountPolicy::Auto if dev.mount_point.is_none() => {
                if let Err(e) = self.mount(id).await {
                    tracing::warn!(%e, device = %dev.device, "auto-mount failed");
                }
            }
            MountPolicy::Ask if dev.mount_point.is_none() => {
                self.mounts.state.lock().unwrap().asking.insert(id.to_owned());
                // The question is shown in browser windows; make sure there is one.
                if !self.hub.has_role(Role::Browser) {
                    let _ = self.supervisor.open_browser(None, None, None);
                }
            }
            _ => {}
        }
    }

    pub async fn mount(&self, id: &str) -> anyhow::Result<PathBuf> {
        let conn = self.mounts.conn()?;
        let opts: HashMap<&str, Value> = HashMap::new();
        let reply = conn.call_method(Some(UDISKS), id, Some(FILESYSTEM), "Mount", &(opts,)).await?;
        let path: String = reply.body().deserialize()?;
        self.mounts.state.lock().unwrap().asking.remove(id);
        self.rescan().await?;
        self.hub.broadcast(Event::DevicesChanged);
        tracing::info!(%path, "mounted");
        Ok(PathBuf::from(path))
    }

    pub async fn unmount(&self, id: &str) -> anyhow::Result<()> {
        let conn = self.mounts.conn()?;
        let opts: HashMap<&str, Value> = HashMap::new();
        conn.call_method(Some(UDISKS), id, Some(FILESYSTEM), "Unmount", &(opts,)).await?;
        self.rescan().await?;
        self.hub.broadcast(Event::DevicesChanged);
        Ok(())
    }

    pub async fn set_mount_policy(&self, uuid: String, policy: MountPolicy) -> anyhow::Result<()> {
        let answering: Vec<String> = {
            let mut s = self.mounts.state.lock().unwrap();
            s.policies.insert(uuid.clone(), policy);
            let ids: Vec<String> = s.raw.iter().filter(|b| b.uuid == uuid).map(|b| b.id.clone()).collect();
            ids.into_iter().filter(|id| s.asking.remove(id)).collect()
        };
        self.mounts.save_policies();
        // "Always mount" given as the answer to a question mounts right away.
        if policy == MountPolicy::Auto {
            for id in answering {
                self.mount(&id).await?;
            }
        }
        self.hub.broadcast(Event::DevicesChanged);
        Ok(())
    }

    pub fn dismiss_ask(&self, id: &str) {
        self.mounts.state.lock().unwrap().asking.remove(id);
        self.hub.broadcast(Event::DevicesChanged);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(id: &str, f: impl FnOnce(&mut RawBlock)) -> RawBlock {
        let mut b = RawBlock {
            id: id.into(),
            device: format!("/dev/{id}"),
            uuid: format!("uuid-{id}"),
            fs_type: "ext4".into(),
            usage: "filesystem".into(),
            has_filesystem: true,
            size: 1 << 30,
            ..Default::default()
        };
        f(&mut b);
        b
    }

    #[test]
    fn filters_and_labels() {
        let raw = vec![
            block("root", |b| {
                b.hint_system = true;
                b.mount_points = vec!["/".into(), "/home".into()];
            }),
            block("boot", |b| {
                b.hint_system = true;
                b.mount_points = vec!["/boot".into()];
            }),
            block("swap", |b| b.usage = "other".into()),
            block("hidden", |b| b.hint_ignore = true),
            block("winc", |b| {
                b.hint_system = true;
                b.fs_type = "ntfs".into();
            }),
            block("usb", |b| {
                b.label = "STICK".into();
                b.drive_removable = true;
                b.drive_model = "Cruzer".into();
                b.mount_points = vec!["/run/media/u/STICK".into()];
            }),
            block("raw", |b| b.has_filesystem = false),
            block("iso", |b| {
                b.hint_system = true;
                b.user_loop = true;
            }),
        ];
        let policies = HashMap::from([("uuid-winc".to_owned(), MountPolicy::Auto)]);
        let asking = HashSet::from(["usb".to_owned()]);
        let devs = to_devices(&raw, &policies, &asking);
        let ids: Vec<_> = devs.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, ["iso", "usb", "winc"], "plugged-in first; system mounts and non-filesystems hidden");

        assert!(!devs[0].internal, "user-attached images count as plugged in");
        let usb = &devs[1];
        assert_eq!(usb.label.as_deref(), Some("STICK"));
        assert_eq!(usb.mount_point.as_deref(), Some(Path::new("/run/media/u/STICK")));
        assert!(!usb.internal && usb.asking);
        assert_eq!(usb.policy, MountPolicy::Ask, "unknown removable devices ask");

        let winc = &devs[2];
        assert!(winc.internal && winc.label.is_none());
        assert_eq!(winc.policy, MountPolicy::Auto, "saved policy wins");
        let internal_default = to_devices(&raw, &HashMap::new(), &HashSet::new());
        assert_eq!(internal_default[2].policy, MountPolicy::Never, "internal partitions default to never");
    }
}
