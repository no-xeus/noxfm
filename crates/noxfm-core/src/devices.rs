//! Devices and places as the sidebar shows them: partitions grouped by
//! disk, names, icons and mount-rule labels. No UI toolkit.

use noxfm_proto::{Device, MountPolicy};

/// By drive, which udisks names after model and serial, so it survives
/// reboots and replugging. Disk images have no drive: their device stands in.
pub fn disk_key(d: &Device) -> String {
    format!("disk:{}", d.drive_id.as_deref().unwrap_or(&d.id))
}

pub fn policy_label(p: MountPolicy) -> &'static str {
    match p {
        MountPolicy::Ask => "Ask when plugged in",
        MountPolicy::Auto => "Mount automatically",
        MountPolicy::Never => "Never mount",
    }
}

/// The rule in one word, shown under unmounted partitions.
pub fn policy_short(p: MountPolicy) -> &'static str {
    match p {
        MountPolicy::Ask => "ask",
        MountPolicy::Auto => "auto-mount",
        MountPolicy::Never => "never",
    }
}

/// "yepeeee", or "Partition 2" / the device name when there's no label.
pub fn partition_name(d: &Device) -> String {
    match (&d.label, d.partition) {
        (Some(l), _) => l.clone(),
        (None, Some(n)) => format!("Partition {n}"),
        (None, None) => d.device.rsplit('/').next().unwrap_or(&d.device).to_owned(),
    }
}

/// A physical disk (or a disk image) and its listed partitions, with each
/// partition's index in the device list.
pub struct Disk<'a> {
    pub key: String,
    pub name: String,
    pub size: u64,
    pub external: bool,
    pub image: bool,
    pub parts: Vec<(usize, &'a Device)>,
}

impl Disk<'_> {
    pub fn icon(&self) -> &'static [&'static str] {
        kind_icon(self.image, self.external)
    }
}

fn kind_icon(image: bool, external: bool) -> &'static [&'static str] {
    match (image, external) {
        (true, _) => &["media-optical", "drive-removable-media"],
        (false, true) => &["drive-removable-media-usb", "drive-removable-media"],
        (false, false) => &["drive-harddisk"],
    }
}

/// A partition looks like the kind of disk it's on.
pub fn device_icon(d: &Device) -> &'static [&'static str] {
    kind_icon(d.drive_id.is_none(), !d.internal)
}

/// Groups partitions by disk, keeping the daemon's order.
pub fn group_disks(devices: &[Device]) -> Vec<Disk<'_>> {
    let mut disks: Vec<Disk<'_>> = Vec::new();
    for (i, d) in devices.iter().enumerate() {
        let key = disk_key(d);
        match disks.iter_mut().find(|disk| disk.key == key) {
            Some(disk) => disk.parts.push((i, d)),
            None => {
                let image = d.drive_id.is_none();
                let name = match (&d.drive, image) {
                    (Some(model), _) => model.clone(),
                    (None, true) => "Disk image".into(),
                    (None, false) => "Disk".into(),
                };
                let size = if d.drive_size > 0 { d.drive_size } else { d.size };
                disks.push(Disk { key, name, size, external: !d.internal, image, parts: vec![(i, d)] });
            }
        }
    }
    disks
}

pub fn find_disk<'a>(devices: &'a [Device], key: &str) -> Option<Disk<'a>> {
    group_disks(devices).into_iter().find(|d| d.key == key)
}

pub fn place_icon(name: &str) -> &'static str {
    match name {
        "Home" => "user-home",
        "Downloads" => "folder-download",
        "Documents" => "folder-documents",
        "Pictures" => "folder-pictures",
        "Videos" => "folder-videos",
        "Music" => "folder-music",
        "Desktop" => "user-desktop",
        _ => "folder",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(id: &str, drive: Option<&str>, part: Option<u32>, internal: bool) -> Device {
        Device {
            id: id.into(),
            device: format!("/dev/{id}"),
            label: None,
            uuid: None,
            fs_type: Some("ext4".into()),
            size: 100,
            mount_point: None,
            drive: drive.map(|d| format!("Model {d}")),
            drive_id: drive.map(String::from),
            drive_size: 1000,
            partition: part,
            internal,
            policy: MountPolicy::Ask,
            asking: false,
            free: None,
        }
    }

    #[test]
    fn groups_partitions_under_disks() {
        let devices = vec![
            dev("sdb1", Some("usb"), Some(1), false),
            dev("sdb2", Some("usb"), Some(2), false),
            dev("loop0", None, None, false),
            dev("sda1", Some("ssd"), Some(1), true),
        ];
        let disks = group_disks(&devices);
        let shape: Vec<(String, bool, Vec<usize>)> =
            disks.iter().map(|d| (d.name.clone(), d.external, d.parts.iter().map(|(i, _)| *i).collect())).collect();
        assert_eq!(
            shape,
            [
                ("Model usb".into(), true, vec![0, 1]),
                ("Disk image".into(), true, vec![2]),
                ("Model ssd".into(), false, vec![3]),
            ]
        );
        assert_eq!(disks[0].size, 1000, "a disk's size is the whole disk");
        assert_eq!(partition_name(&devices[1]), "Partition 2");
        assert_eq!(partition_name(&devices[2]), "loop0");
        assert!(find_disk(&devices, "disk:ssd").is_some());
    }

    #[test]
    fn keys_and_icons() {
        let d = dev("sdb1", Some("/org/drives/Cruzer_123"), Some(1), false);
        assert_eq!(disk_key(&d), "disk:/org/drives/Cruzer_123");
        assert_eq!(disk_key(&dev("loop0", None, None, false)), "disk:loop0");
        assert_eq!(device_icon(&d)[0], "drive-removable-media-usb");
        assert_eq!(device_icon(&dev("sda1", Some("x"), Some(1), true))[0], "drive-harddisk");
        assert_eq!(device_icon(&dev("loop0", None, None, false))[0], "media-optical");
    }

}
