//! Left sidebar (Recent, Places, Devices) and the "mount this?" banners.
//!
//! Every section and every disk can be folded by clicking its header. What's
//! folded is remembered by the daemon (shared by all windows) under a stable
//! key: `section:<name>` or the disk's key.

use std::collections::HashMap;

use cosmic::iced::{Alignment, Length};
use cosmic::prelude::*;
use cosmic::widget::{self, menu};
use noxfm_proto::{Device, MountPolicy, Place, RecentKind};

use super::{App, Message};
use crate::{fmt, icons};
pub use noxfm_core::devices::*;

pub const SIDEBAR_W: f32 = 240.0;

/// Right-click menu of a place. Holds its index in the places list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaceAction {
    OpenNewWindow(usize),
    Unpin(usize),
}

impl menu::Action for PlaceAction {
    type Message = Message;

    fn message(&self) -> Message {
        Message::PlaceMenu(*self)
    }
}

fn place_menu(i: usize, p: &Place) -> Vec<menu::Tree<Message>> {
    let mut items = vec![menu::Item::Button("Open in new window", None, PlaceAction::OpenNewWindow(i))];
    if p.pinned {
        items.push(menu::Item::Divider);
        items.push(menu::Item::Button("Unpin from sidebar", None, PlaceAction::Unpin(i)));
    }
    menu::items(&HashMap::new(), items)
}

/// What a partition's menu (and clicks on it) can do. Holds the device's
/// index in the list: menu actions must be `Copy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceAction {
    /// Open, mounting first if needed.
    Open(usize),
    Unmount(usize),
    Policy(usize, MountPolicy),
    CopyPath(usize),
    Properties(usize),
}

impl menu::Action for DeviceAction {
    type Message = Message;

    fn message(&self) -> Message {
        Message::DeviceMenu(*self)
    }
}

/// A disk header's menu. Holds the index of one of the disk's partitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskAction {
    Properties(usize),
}

impl menu::Action for DiskAction {
    type Message = Message;

    fn message(&self) -> Message {
        Message::DiskMenu(*self)
    }
}

fn device_menu(i: usize, d: &Device) -> Vec<menu::Tree<Message>> {
    let mounted = d.mount_point.is_some();
    let mut items = vec![
        menu::Item::Button(if mounted { "Open" } else { "Mount and open" }, None, DeviceAction::Open(i)),
        if mounted {
            menu::Item::Button("Unmount", None, DeviceAction::Unmount(i))
        } else {
            menu::Item::ButtonDisabled("Unmount", None, DeviceAction::Unmount(i))
        },
    ];
    // The rule is stored per filesystem UUID; without one there's nothing to remember.
    if d.uuid.is_some() {
        items.push(menu::Item::Divider);
        for p in [MountPolicy::Ask, MountPolicy::Auto, MountPolicy::Never] {
            items.push(menu::Item::CheckBox(policy_label(p), None, d.policy == p, DeviceAction::Policy(i, p)));
        }
    }
    items.push(menu::Item::Divider);
    items.push(menu::Item::Button("Copy device path", None, DeviceAction::CopyPath(i)));
    items.push(menu::Item::Divider);
    items.push(menu::Item::Button("Properties", None, DeviceAction::Properties(i)));
    menu::items(&HashMap::new(), items)
}

fn disk_menu(first_part: usize) -> Vec<menu::Tree<Message>> {
    menu::items(&HashMap::new(), vec![menu::Item::Button("Properties", None, DiskAction::Properties(first_part))])
}

/// A section title that folds its section: chevron + caption.
fn section_header<'a>(title: &'static str, key: &'static str, collapsed: bool) -> Element<'a, Message> {
    let chevron = if collapsed { "pan-end-symbolic" } else { "pan-down-symbolic" };
    widget::button::custom(
        widget::row::with_capacity(2)
            .spacing(4)
            .align_y(Alignment::Center)
            .push(icons::get(&[chevron, "go-next-symbolic"], 12))
            .push(widget::text::caption_heading(title)),
    )
    .class(cosmic::theme::Button::Text)
    .padding([8, 6, 2, 6])
    .width(Length::Fill)
    .on_press(Message::ToggleCollapsed(key.into()))
    .into()
}

impl App {
    pub(super) fn sidebar(&self) -> Element<'_, Message> {
        let space = cosmic::theme::spacing();
        let folded = |key: &str| self.collapsed.contains(key);
        let item = |icon: &[&str], label: String, active: bool, msg: Message| -> Element<'_, Message> {
            let row = widget::row::with_capacity(2)
                .spacing(8)
                .align_y(Alignment::Center)
                .push(icons::get(icon, 16))
                .push(widget::text::body(label));
            widget::button::custom(row)
                .class(if active { cosmic::theme::Button::Standard } else { cosmic::theme::Button::Text })
                .padding([4, 8])
                .width(Length::Fill)
                .on_press(msg)
                .into()
        };

        let mut col = widget::column::with_capacity(40).spacing(2);
        col = col.push(section_header("Recent", "section:recent", folded("section:recent")));
        if !folded("section:recent") {
            for (label, kind, icon) in [
                ("All recent", None, "document-open-recent"),
                ("Downloaded", Some(RecentKind::Downloaded), "folder-download"),
                ("Edited", Some(RecentKind::Modified), "document-edit"),
                ("Created", Some(RecentKind::Created), "document-new"),
            ] {
                col = col.push(item(&[icon, "folder"], label.into(), self.recent == Some(kind), Message::OpenRecent(kind)));
            }
        }

        let place_row = |i: usize, p: &'_ Place| -> Element<'_, Message> {
            let active = self.recent.is_none() && !self.trash_view && self.path == p.path;
            let icon = if p.pinned { "folder" } else { place_icon(&p.name) };
            let row = item(&[icon, "folder"], p.name.clone(), active, Message::Navigate(p.path.clone()));
            self.with_menu(row, place_menu(i, p))
        };

        col = col.push(section_header("Places", "section:places", folded("section:places")));
        if !folded("section:places") {
            for (i, p) in self.places.iter().enumerate().filter(|(_, p)| !p.pinned) {
                col = col.push(place_row(i, p));
            }
            let trash_icon: &[&str] = if self.trash_count > 0 { &["user-trash-full", "user-trash"] } else { &["user-trash"] };
            let trash_label = if self.trash_count > 0 { format!("Trash ({})", self.trash_count) } else { "Trash".into() };
            col = col.push(item(trash_icon, trash_label, self.trash_view, Message::OpenTrash));
        }

        if self.places.iter().any(|p| p.pinned) {
            col = col.push(section_header("Pinned", "section:pinned", folded("section:pinned")));
            if !folded("section:pinned") {
                for (i, p) in self.places.iter().enumerate().filter(|(_, p)| p.pinned) {
                    col = col.push(place_row(i, p));
                }
            }
        }

        for (external, title, key) in [(true, "Plugged in", "section:plugged"), (false, "Internal disks", "section:internal")] {
            let disks: Vec<Disk<'_>> = group_disks(&self.devices).into_iter().filter(|d| d.external == external).collect();
            if disks.is_empty() {
                continue;
            }
            col = col.push(section_header(title, key, folded(key)));
            if folded(key) {
                continue;
            }
            for disk in disks {
                let disk_folded = folded(&disk.key);
                let first = disk.parts[0].0;
                col = col.push(self.with_menu(disk_header(&disk, disk_folded), disk_menu(first)));
                if !disk_folded {
                    for (i, d) in &disk.parts {
                        col = col.push(self.partition_row(*i, d));
                    }
                }
            }
        }

        widget::container(widget::scrollable(col.padding([0, space.space_xxs])))
            .width(self.sidebar_width)
            .height(Length::Fill)
            .into()
    }

    fn with_menu<'a>(&self, content: Element<'a, Message>, menu: Vec<menu::Tree<Message>>) -> Element<'a, Message> {
        let mut m = widget::context_menu(content, Some(menu)).on_surface_action(Message::Surface);
        if let Some(id) = self.core.main_window_id() {
            m = m.window_id(id);
        }
        m.into()
    }

    /// One partition (or volume): click opens it, mounting first if needed;
    /// right-click shows actions, the mount rule, Hide and Properties.
    fn partition_row<'a>(&'a self, i: usize, d: &'a Device) -> Element<'a, Message> {
        let mounted = d.mount_point.is_some();
        let active = self.recent.is_none() && d.mount_point.as_ref().is_some_and(|m| self.path.starts_with(m));
        let mut facts = vec![d.fs_type.clone().unwrap_or_else(|| "volume".into())];
        facts.push(if mounted { "mounted".into() } else { policy_short(d.policy).into() });
        let no_wrap = cosmic::iced::widget::text::Wrapping::None;
        let text = widget::column::with_capacity(2)
            .push(widget::text::body(partition_name(d)).wrapping(no_wrap))
            .push(widget::text::caption(facts.join(" · ")).wrapping(no_wrap));
        let tip = format!("{} · right-click for options", d.device);
        let main = widget::button::custom(
            widget::row::with_capacity(2).spacing(8).align_y(Alignment::Center).push(icons::get(device_icon(d), 16)).push(text),
        )
        .class(if active { cosmic::theme::Button::Standard } else { cosmic::theme::Button::Text })
        .padding([4, 8])
        .width(Length::Fill)
        .on_press(Message::DeviceMenu(DeviceAction::Open(i)));

        let main = widget::tooltip(main, widget::text::body(tip), widget::tooltip::Position::Right);
        let mut row = widget::row::with_capacity(2).align_y(Alignment::Center).push(main);
        if mounted {
            let eject = widget::button::icon(icons::handle(&["media-eject-symbolic"], 16))
                .on_press(Message::DeviceMenu(DeviceAction::Unmount(i)));
            row = row.push(widget::tooltip(eject, widget::text::body("Unmount"), widget::tooltip::Position::Bottom));
        }
        // Indented under the disk header.
        let row = widget::container(row).padding([0, 0, 0, 20]);
        self.with_menu(row.into(), device_menu(i, d))
    }

    /// One banner per device waiting for "mount it?".
    pub(super) fn ask_banners(&self) -> Option<Element<'_, Message>> {
        let asking: Vec<&Device> = self.devices.iter().filter(|d| d.asking).collect();
        if asking.is_empty() {
            return None;
        }
        let col = asking.into_iter().fold(widget::column::with_capacity(2).spacing(4), |c, d| {
            let mut row = widget::row::with_capacity(6)
                .spacing(8)
                .align_y(Alignment::Center)
                .push(icons::get(device_icon(d), 24))
                .push(
                    widget::text::body(format!("“{}” ({}) was plugged in. Mount it?", partition_name(d), fmt::size(d.size)))
                        .width(Length::Fill),
                )
                .push(widget::button::suggested("Mount").on_press(Message::MountDevice(d.id.clone())));
            if let Some(uuid) = &d.uuid {
                row = row
                    .push(widget::button::standard("Always").on_press(Message::SetPolicy(uuid.clone(), MountPolicy::Auto)))
                    .push(widget::button::standard("Never").on_press(Message::SetPolicy(uuid.clone(), MountPolicy::Never)));
            }
            row = row.push(widget::button::text("Not now").on_press(Message::DismissAsk(d.id.clone())));
            c.push(widget::container(row).padding(8).class(cosmic::style::Container::Card))
        });
        Some(col.into())
    }
}

/// The disk's name; clicking it folds or unfolds its partitions.
fn disk_header<'a>(disk: &Disk<'_>, collapsed: bool) -> Element<'a, Message> {
    let chevron = if collapsed { "pan-end-symbolic" } else { "pan-down-symbolic" };
    widget::button::custom(
        widget::row::with_capacity(3)
            .spacing(6)
            .align_y(Alignment::Center)
            .push(icons::get(&[chevron, "go-next-symbolic"], 12))
            .push(icons::get(disk.icon(), 20))
            .push(widget::text::heading(disk.name.clone())),
    )
    .class(cosmic::theme::Button::Text)
    .padding([6, 6, 2, 6])
    .width(Length::Fill)
    .on_press(Message::ToggleCollapsed(disk.key.clone()))
    .into()
}
