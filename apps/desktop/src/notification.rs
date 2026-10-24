use crate::scroll_area::ScrollbarExt;
use gpui::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

const WIDTH: f32 = 360.;
const HEIGHT: f32 = 144.;
const FADE_DURATION: Duration = Duration::from_millis(180);
const TIMER_INTERVAL: Duration = Duration::from_millis(100);

pub struct NotificationContent {
    pub title: String,
    pub message: String,
    pub full_text: String,
    pub copy_label: &'static str,
    pub copied_label: &'static str,
    pub duration_seconds: u64,
}

pub struct DesktopNotification {
    title: String,
    message: String,
    full_text: String,
    copy_label: &'static str,
    copied_label: &'static str,
    copied: bool,
    closing: bool,
    hovered: Arc<AtomicBool>,
}

impl DesktopNotification {
    pub const fn width() -> f32 {
        WIDTH
    }

    pub const fn height() -> f32 {
        HEIGHT
    }

    pub fn new(window: &mut Window, cx: &mut Context<Self>, content: NotificationContent) -> Self {
        let hovered = Arc::new(AtomicBool::new(false));
        let timer_hovered = Arc::clone(&hovered);
        let display_duration = Duration::from_secs(content.duration_seconds);
        cx.spawn_in(window, async move |this, cx| {
            let mut remaining = display_duration;
            while !remaining.is_zero() {
                cx.background_executor().timer(TIMER_INTERVAL).await;
                if !timer_hovered.load(Ordering::Relaxed) {
                    remaining = remaining.saturating_sub(TIMER_INTERVAL);
                }
            }
            let _ = this.update_in(cx, |this, window, cx| this.dismiss(window, cx));
        })
        .detach();

        Self {
            title: content.title,
            message: content.message,
            full_text: content.full_text,
            copy_label: content.copy_label,
            copied_label: content.copied_label,
            copied: false,
            closing: false,
            hovered,
        }
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.closing {
            return;
        }
        self.closing = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(FADE_DURATION).await;
            let _ = this.update_in(cx, |_, window, _| window.remove_window());
        })
        .detach();
    }
}

impl Render for DesktopNotification {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let hovered = Arc::clone(&self.hovered);
        let closing = self.closing;
        let view = div()
            .id("desktop-receive-notification")
            .size_full()
            .rounded(px(16.))
            .border_1()
            .border_color(rgba(0xffffff28))
            .bg(rgb(0x202024))
            .p(px(14.))
            .flex()
            .flex_col()
            .gap(px(7.))
            .text_color(rgb(0xf5f5f7))
            .font_family("Segoe UI")
            .on_hover(move |is_hovered, _, _| {
                hovered.store(*is_hovered, Ordering::Relaxed);
            })
            .child(
                div()
                    .h(px(22.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().size(px(7.)).rounded_full().bg(rgb(0x30d158)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(13.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(self.title.clone()),
                    )
                    .child(
                        div()
                            .id("dismiss-desktop-notification")
                            .size(px(24.))
                            .flex_shrink_0()
                            .rounded(px(6.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .text_size(px(18.))
                            .text_color(rgb(0xa0a0a8))
                            .hover(|style| style.bg(rgb(0x38383d)))
                            .child("×")
                            .on_click(cx.listener(|this, _, window, cx| this.dismiss(window, cx))),
                    ),
            )
            .child(
                div()
                    .id("desktop-notification-message")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .visible_scrollbar()
                    .whitespace_normal()
                    .text_size(px(11.))
                    .line_height(px(16.))
                    .text_color(rgb(0x30d158))
                    .child(self.message.clone()),
            )
            .child(
                div()
                    .h(px(24.))
                    .flex_shrink_0()
                    .flex()
                    .justify_end()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .id("copy-desktop-notification")
                            .h(px(24.))
                            .px(px(8.))
                            .rounded(px(6.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .text_size(px(10.))
                            .text_color(rgb(if self.copied { 0x30d158 } else { 0xa0a0a8 }))
                            .hover(|style| style.bg(rgb(0x38383d)))
                            .child(if self.copied {
                                self.copied_label
                            } else {
                                self.copy_label
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(
                                    this.full_text.clone(),
                                ));
                                this.copied = true;
                                cx.notify();
                            })),
                    ),
            );
        view.with_animation(
            ("desktop-notification-opacity", u64::from(closing)),
            Animation::new(FADE_DURATION).with_easing(ease_in_out),
            move |element, progress| {
                element.opacity(if closing { 1. - progress } else { progress })
            },
        )
    }
}
