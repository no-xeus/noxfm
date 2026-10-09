//! Properties: floating panels over the file list. Each opens centred, is
//! dragged by its header and stays inside the window. Several can be open.

use std::path::{Path, PathBuf};

use cosmic::iced::{Alignment, Length, Point, Size, Vector};
use cosmic::prelude::*;
use cosmic::widget;
use noxfm_proto::{EntryKind, Props, Request, Response};

use super::{App, Message};
use crate::{fmt, icons};

const PANEL_W: f32 = 400.0;
const PANEL_H: f32 = 500.0;
/// Each new panel is offset a little from the previous one.
const CASCADE: f32 = 24.0;

pub struct PropsPanel {
    pub id: u64,
    /// From the centred position.
    pub offset: Vector,
    pub kind: PanelKind,
}

pub enum PanelKind {
    Files { paths: Vec<PathBuf>, data: Option<Result<Box<Props>, String>> },
    /// A partition, by udisks id. Shown from the live device list.
    Partition(String),
    /// A whole disk, by sidebar disk key.
    Disk(String),
}

/// A header drag in progress.
pub struct PanelDrag {
    pub id: u64,
    pub start_pointer: Point,
    pub start_offset: Vector,
}

#[derive(Debug, Clone)]
pub enum PropsMsg {
    Loaded(u64, Result<Box<Props>, String>),
    Close(u64),
    DragStart(u64),
    /// Toggles one permission bit (0o400, 0o200, … 0o001).
    Toggle(u64, u32),
    ChangeApp(u64),
}

impl App {
    pub(super) fn open_properties(&mut self, paths: Vec<PathBuf>) -> cosmic::app::Task<Message> {
        let id = self.push_panel(PanelKind::Files { paths: paths.clone(), data: None });
        self.load_props(id, paths)
    }

    /// Opens (or brings forward) the Properties of a partition or disk.
    pub(super) fn open_device_properties(&mut self, kind: PanelKind) {
        let same = |k: &PanelKind| match (k, &kind) {
            (PanelKind::Partition(a), PanelKind::Partition(b)) | (PanelKind::Disk(a), PanelKind::Disk(b)) => a == b,
            _ => false,
        };
        if let Some(i) = self.props.iter().position(|p| same(&p.kind)) {
            let panel = self.props.remove(i);
            self.props.push(panel);
        } else {
            self.push_panel(kind);
        }
    }

    fn push_panel(&mut self, kind: PanelKind) -> u64 {
        self.next_panel += 1;
        let n = self.props.len() as f32;
        self.props.push(PropsPanel { id: self.next_panel, offset: Vector::new(n * CASCADE, n * CASCADE), kind });
        self.next_panel
    }

    fn file_data(&self, id: u64) -> Option<(&Vec<PathBuf>, &Props)> {
        match &self.props.iter().find(|p| p.id == id)?.kind {
            PanelKind::Files { paths, data: Some(Ok(props)) } => Some((paths, props)),
            _ => None,
        }
    }

    fn load_props(&self, id: u64, paths: Vec<PathBuf>) -> cosmic::app::Task<Message> {
        let Some(client) = self.client.clone() else { return Task::none() };
        cosmic::task::future(async move {
            let r = match client.request(Request::Properties { paths }).await {
                Ok(Response::Properties(p)) => Ok(p),
                Ok(other) => Err(format!("unexpected reply: {other:?}")),
                Err(e) => Err(e.to_string()),
            };
            Message::Props(PropsMsg::Loaded(id, r))
        })
    }

    pub(super) fn props_update(&mut self, m: PropsMsg) -> cosmic::app::Task<Message> {
        match m {
            PropsMsg::Loaded(id, r) => {
                if let Some(PanelKind::Files { data, .. }) = self.props.iter_mut().find(|p| p.id == id).map(|p| &mut p.kind) {
                    *data = Some(r);
                }
            }
            PropsMsg::Close(id) => self.props.retain(|p| p.id != id),
            PropsMsg::DragStart(id) => {
                // Bring to front, then follow the pointer.
                if let Some(i) = self.props.iter().position(|p| p.id == id) {
                    let panel = self.props.remove(i);
                    if let Some(pointer) = self.pointer {
                        self.panel_drag = Some(PanelDrag { id, start_pointer: pointer, start_offset: panel.offset });
                    }
                    self.props.push(panel);
                }
            }
            PropsMsg::Toggle(id, bit) => {
                let Some((paths, Props { entry: Some(e), .. })) = self.file_data(id) else { return Task::none() };
                let (path, mode) = (e.path.clone(), (e.mode ^ bit) & 0o7777);
                let paths = paths.clone();
                let reload = self.load_props(id, paths);
                return Task::batch([self.request(Request::Chmod { path, mode }), reload]);
            }
            PropsMsg::ChangeApp(id) => {
                let Some((_, Props { entry: Some(e), .. })) = self.file_data(id) else { return Task::none() };
                let mime = e.mime.clone().unwrap_or_else(|| "application/octet-stream".into());
                return self.open_app_picker(mime, None);
            }
        }
        Task::none()
    }

    /// Moves the dragged panel; keeps it inside the items area.
    pub(super) fn drag_panel(&mut self, pointer: Point) {
        let Some(d) = &self.panel_drag else { return };
        let area = self.items_size.get();
        let Some(panel) = self.props.iter_mut().find(|p| p.id == d.id) else { return };
        let mut off = d.start_offset + (pointer - d.start_pointer);
        let (cx, cy) = centre_origin(area);
        off.x = off.x.clamp(-cx, (area.width - PANEL_W - cx).max(-cx));
        off.y = off.y.clamp(-cy, (area.height - 48.0 - cy).max(-cy));
        panel.offset = off;
    }

    /// The panels, positioned over the items area (later ones on top).
    pub(super) fn panels(&self) -> Vec<Element<'_, Message>> {
        let area = self.items_size.get();
        let (cx, cy) = centre_origin(area);
        self.props
            .iter()
            .map(|p| {
                let left = (cx + p.offset.x).max(0.0);
                let top = (cy + p.offset.y).max(0.0);
                widget::container(self.panel(p))
                    .padding(cosmic::iced::Padding { top, left, right: 0.0, bottom: 0.0 })
                    .into()
            })
            .collect()
    }

    fn panel<'a>(&'a self, p: &'a PropsPanel) -> Element<'a, Message> {
        let (title, icon, body): (String, Element<'a, Message>, Element<'a, Message>) = match &p.kind {
            PanelKind::Files { paths, data } => {
                let title = match paths.as_slice() {
                    [one] => name_of(one),
                    many => format!("{} items", many.len()),
                };
                let icon = match data.as_ref().and_then(|d| d.as_ref().ok()).and_then(|p| p.entry.as_ref()) {
                    Some(e) => self.entry_icon(e, 32),
                    None => icons::get(&["document-properties", "text-x-generic"], 32).into(),
                };
                let body = match data {
                    None => widget::text::body("Calculating…").into(),
                    Some(Err(e)) => widget::text::body(e.as_str()).into(),
                    Some(Ok(props)) => widget::scrollable(self.props_body(p.id, props)).into(),
                };
                (title, icon, body)
            }
            PanelKind::Partition(dev_id) => match self.devices.iter().find(|d| d.id == *dev_id) {
                Some(d) => (
                    super::sidebar::partition_name(d),
                    icons::get(super::sidebar::device_icon(d), 32).into(),
                    widget::scrollable(partition_body(d)).into(),
                ),
                None => ("Partition".into(), icons::get(&["drive-harddisk"], 32).into(), widget::text::body("This device is no longer connected.").into()),
            },
            PanelKind::Disk(key) => match super::sidebar::find_disk(&self.devices, key) {
                Some(disk) => (disk.name.clone(), icons::get(disk.icon(), 32).into(), widget::scrollable(disk_body(&disk)).into()),
                None => ("Disk".into(), icons::get(&["drive-harddisk"], 32).into(), widget::text::body("This disk is no longer connected.").into()),
            },
        };
        let header = widget::row::with_capacity(3)
            .spacing(10)
            .align_y(Alignment::Center)
            .push(icon)
            .push(widget::text::heading(title).width(Length::Fill))
            .push(widget::button::icon(icons::handle(&["window-close-symbolic"], 16)).on_press(Message::Props(PropsMsg::Close(p.id))));
        let header = widget::mouse_area(widget::container(header).padding([8, 10]).width(Length::Fill))
            .on_press(Message::Props(PropsMsg::DragStart(p.id)))
            .interaction(cosmic::iced::mouse::Interaction::Grab);

        let card = widget::container(
            widget::column::with_capacity(3)
                .push(header)
                .push(widget::divider::horizontal::default())
                .push(widget::container(body).padding([8, 12]).height(Length::Fill)),
        )
        .width(PANEL_W)
        .height(PANEL_H)
        // Drawn like a dialog floating over the window.
        .class(cosmic::style::Container::Dialog(true));
        // Swallow clicks so they don't reach the files underneath.
        widget::mouse_area(card)
            .on_press(Message::Noop)
            .on_right_press(Message::RightPressConsumed)
            .into()
    }

    fn props_body<'a>(&'a self, id: u64, props: &'a Props) -> Element<'a, Message> {
        let mut col = widget::column::with_capacity(16).spacing(6);
        let row = |label: &'static str, value: String| -> Element<'a, Message> {
            widget::row::with_capacity(2)
                .spacing(12)
                .push(widget::text::caption_heading(label).width(Length::Fixed(110.0)))
                .push(widget::text::body(value).width(Length::Fill))
                .into()
        };
        let size = |bytes: u64| format!("{} ({} bytes)", fmt::size(bytes), fmt::group_digits(bytes));

        match &props.entry {
            Some(e) => {
                let mut kind = e.mime.clone().unwrap_or_else(|| "unknown".into());
                if let Some(ext) = props.ext_mime.as_ref().filter(|_| e.mime_mismatch) {
                    kind += &format!("\n⚠ content is {}, the extension says {ext}", e.mime.as_deref().unwrap_or("?"));
                }
                col = col.push(row("Type", if e.kind == EntryKind::Dir { "Folder".into() } else { kind }));
                if let Some(app) = &e.app {
                    col = col.push(
                        widget::row::with_capacity(3)
                            .spacing(12)
                            .align_y(Alignment::Center)
                            .push(widget::text::caption_heading("Opens with").width(Length::Fixed(110.0)))
                            .push(widget::text::body(app.name.as_str()).width(Length::Fill))
                            .push(widget::button::standard("Change…").on_press(Message::Props(PropsMsg::ChangeApp(id)))),
                    );
                }
                let location = e.path.parent().map(fmt::short_path).unwrap_or_default();
                col = col.push(row("Location", fmt::ellipsize_start(&location, 34)));
                if e.symlink {
                    let target = std::fs::read_link(&e.path).map(|t| t.display().to_string()).unwrap_or_default();
                    col = col.push(row("Link to", target));
                }
                col = col.push(widget::divider::horizontal::light());
                if e.kind == EntryKind::Dir {
                    col = col.push(row("Size", size(props.bytes)));
                    col = col.push(row("Contains", contains(props)));
                } else {
                    col = col.push(row("Size", size(e.size.unwrap_or(props.bytes))));
                }
                col = col.push(widget::divider::horizontal::light());
                col = col.push(row("Created", fmt::time(e.created)));
                col = col.push(row("Modified", fmt::time(e.modified)));
                col = col.push(row("Accessed", fmt::time(props.accessed)));
                col = col.push(widget::divider::horizontal::light());
                let owner = format!(
                    "{} / {}",
                    e.owner.clone().unwrap_or_else(|| e.uid.to_string()),
                    e.group.clone().unwrap_or_else(|| e.gid.to_string())
                );
                col = col.push(row("Owner / group", owner));
                col = col.push(self.permissions(id, e.mode));
                if let Some(fs) = &props.fs {
                    col = col.push(row("Filesystem", fs.clone()));
                }
                if let Some(git) = &props.git {
                    let branch = git.branch.clone().unwrap_or_else(|| "detached HEAD".into());
                    let state = if git.dirty { "uncommitted changes" } else { "clean" };
                    col = col.push(row("Git", format!("{branch} · {state}")));
                }
            }
            None => {
                col = col.push(row("Items", format!("{} selected", props.paths.len())));
                let common = props.paths.first().and_then(|p| p.parent()).filter(|d| props.paths.iter().all(|p| p.parent() == Some(*d)));
                if let Some(dir) = common {
                    col = col.push(row("Location", fmt::ellipsize_start(&fmt::short_path(dir), 34)));
                }
                col = col.push(row("Size", size(props.bytes)));
                col = col.push(row("Contains", contains(props)));
            }
        }
        if props.partial {
            col = col.push(widget::text::caption("Some folders couldn't be read; totals are a minimum."));
        }
        col.into()
    }

    /// Owner/group/others × read/write/execute, each a checkbox.
    fn permissions<'a>(&'a self, id: u64, mode: u32) -> Element<'a, Message> {
        let mut grid = widget::column::with_capacity(4).spacing(2).push(
            widget::row::with_capacity(4)
                .push(widget::text::caption_heading("Permissions").width(Length::Fixed(110.0)))
                .push(widget::text::caption("Read").width(Length::Fixed(56.0)))
                .push(widget::text::caption("Write").width(Length::Fixed(56.0)))
                .push(widget::text::caption("Run").width(Length::Fixed(56.0))),
        );
        for (who, shift) in [("Owner", 6), ("Group", 3), ("Others", 0)] {
            let mut r = widget::row::with_capacity(4).align_y(Alignment::Center).push(widget::text::body(who).width(Length::Fixed(110.0)));
            for bit in [4u32, 2, 1] {
                let mask = bit << shift;
                r = r.push(
                    widget::container(widget::checkbox(mode & mask != 0).on_toggle(move |_| Message::Props(PropsMsg::Toggle(id, mask))))
                        .width(Length::Fixed(56.0)),
                );
            }
            grid = grid.push(r);
        }
        grid.push(widget::text::caption(format!("{} ({})", noxfm_core::perms::symbolic(mode), noxfm_core::perms::octal(mode))))
            .into()
    }
}

fn info_row<'a>(label: &'static str, value: String) -> Element<'a, Message> {
    widget::row::with_capacity(2)
        .spacing(12)
        .push(widget::text::caption_heading(label).width(Length::Fixed(110.0)))
        .push(widget::text::body(value).width(Length::Fill))
        .into()
}

fn partition_body<'a>(d: &noxfm_proto::Device) -> Element<'a, Message> {
    let mut col = widget::column::with_capacity(14).spacing(6);
    col = col.push(info_row("Device", d.device.clone()));
    col = col.push(info_row("Filesystem", d.fs_type.clone().unwrap_or_else(|| "unknown".into())));
    if let Some(l) = &d.label {
        col = col.push(info_row("Label", l.clone()));
    }
    if let Some(u) = &d.uuid {
        col = col.push(info_row("UUID", u.clone()));
    }
    col = col.push(widget::divider::horizontal::light());
    col = col.push(info_row("Size", format!("{} ({} bytes)", fmt::size(d.size), fmt::group_digits(d.size))));
    match (&d.mount_point, d.free) {
        (Some(mp), free) => {
            if let Some(free) = free {
                let used = d.size.saturating_sub(free);
                col = col.push(info_row("Used", fmt::size(used)));
                col = col.push(info_row("Free", fmt::size(free)));
                let fraction = if d.size == 0 { 0.0 } else { used as f32 / d.size as f32 };
                col = col.push(widget::progress_bar::linear::Linear::new().progress(fraction).girth(8.0).width(Length::Fill));
            }
            col = col.push(info_row("Mounted at", fmt::short_path(mp)));
        }
        (None, _) => col = col.push(info_row("Mounted", "No".into())),
    }
    col = col.push(widget::divider::horizontal::light());
    col = col.push(info_row("When plugged in", super::sidebar::policy_label(d.policy).into()));
    if let Some(model) = &d.drive {
        let n = d.partition.map(|n| format!(", partition {n}")).unwrap_or_default();
        col = col.push(info_row("Disk", format!("{model}{n}")));
    }
    col = col.push(info_row("Connection", if d.internal { "Internal" } else { "Plugged in" }.into()));
    col.into()
}

fn disk_body<'a>(disk: &super::sidebar::Disk<'_>) -> Element<'a, Message> {
    let mut col = widget::column::with_capacity(10).spacing(6);
    col = col.push(info_row("Size", format!("{} ({} bytes)", fmt::size(disk.size), fmt::group_digits(disk.size))));
    let connection = if disk.image { "Disk image" } else if disk.external { "Plugged in" } else { "Internal" };
    col = col.push(info_row("Connection", connection.into()));
    col = col.push(widget::divider::horizontal::light());
    col = col.push(widget::text::caption_heading("Partitions"));
    for (_, d) in &disk.parts {
        let fs = d.fs_type.clone().unwrap_or_else(|| "?".into());
        let state = match &d.mount_point {
            Some(m) => format!("mounted at {}", fmt::short_path(m)),
            None => "not mounted".into(),
        };
        col = col.push(info_row("", format!("{} · {fs} · {} · {state}", super::sidebar::partition_name(d), fmt::size(d.size))));
    }
    col.into()
}

/// Top-left of a centred panel.
fn centre_origin(area: Size) -> (f32, f32) {
    (((area.width - PANEL_W) / 2.0).max(0.0), ((area.height - PANEL_H) / 2.0).max(0.0))
}

fn name_of(p: &Path) -> String {
    p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned())
}

fn contains(p: &Props) -> String {
    let plural = |n: u64, one: &str, many: &str| format!("{} {}", fmt::group_digits(n), if n == 1 { one } else { many });
    format!("{}, {}", plural(p.files, "file", "files"), plural(p.folders, "folder", "folders"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn centred() {
        let (x, y) = super::centre_origin(cosmic::iced::Size::new(1000.0, 700.0));
        assert_eq!((x, y), (300.0, 100.0));
        assert_eq!(super::centre_origin(cosmic::iced::Size::new(100.0, 100.0)), (0.0, 0.0));
    }
}
