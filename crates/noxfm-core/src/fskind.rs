use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsKind {
    Btrfs,
    Ext4,
    Xfs,
    Ntfs,
    /// FUSE: ntfs-3g, sshfs, … — can't tell which from the magic alone.
    Fuse,
    Vfat,
    Exfat,
    Tmpfs,
    Other(i64),
}

impl FsKind {
    pub fn of(path: &Path) -> std::io::Result<FsKind> {
        let st = rustix::fs::statfs(path)?;
        Ok(Self::from_magic(st.f_type as i64))
    }

    pub fn from_magic(magic: i64) -> FsKind {
        match magic {
            0x9123_683e => FsKind::Btrfs,
            0xef53 => FsKind::Ext4, // shared by ext2/3/4
            0x5846_5342 => FsKind::Xfs,
            0x7366_746e => FsKind::Ntfs, // ntfs3
            0x6573_5546 => FsKind::Fuse,
            0x4d44 => FsKind::Vfat,
            0x2011_bab0 => FsKind::Exfat,
            0x0102_1994 => FsKind::Tmpfs,
            m => FsKind::Other(m),
        }
    }

    pub fn supports_reflink(self) -> bool {
        matches!(self, FsKind::Btrfs | FsKind::Xfs)
    }

    pub fn name(self) -> &'static str {
        match self {
            FsKind::Btrfs => "btrfs",
            FsKind::Ext4 => "ext4",
            FsKind::Xfs => "xfs",
            FsKind::Ntfs => "ntfs",
            FsKind::Fuse => "fuse",
            FsKind::Vfat => "vfat",
            FsKind::Exfat => "exfat",
            FsKind::Tmpfs => "tmpfs",
            FsKind::Other(_) => "other",
        }
    }
}
