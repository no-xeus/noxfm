//! The transfers tab: jobs (from `noxfm_core::jobs`) with progress.

use cosmic::iced::{Alignment, Length};
use cosmic::prelude::*;
use cosmic::widget;
pub use noxfm_core::jobs::Jobs;
use noxfm_core::jobs::Job;

pub fn view<'a, M: Clone + 'static>(jobs: &'a Jobs, cancel: impl Fn(u64) -> M, dismiss: impl Fn(u64) -> M) -> Element<'a, M> {
    let list = jobs.iter().fold(widget::column::with_capacity(4).spacing(12), |c, j| {
        c.push(job_view(j, cancel(j.status.id), dismiss(j.status.id)))
    });
    widget::scrollable(list).into()
}

fn job_view<M: Clone + 'static>(j: &Job, cancel: M, dismiss: M) -> Element<'_, M> {
    let title = widget::text::heading(j.title());
    let detail = j.detail();

    let bar = widget::progress_bar::linear::Linear::new().progress(j.fraction()).girth(6.0).width(Length::Fill);

    let action = match &j.finished {
        None => widget::button::standard("Cancel").on_press(cancel),
        Some(_) => widget::button::standard("Dismiss").on_press(dismiss),
    };

    widget::row::with_capacity(2)
        .spacing(12)
        .align_y(Alignment::Center)
        .push(
            widget::column::with_capacity(3)
                .spacing(4)
                .push(title)
                .push(bar)
                .push(widget::text::caption(detail))
                .width(Length::Fill),
        )
        .push(action)
        .into()
}

