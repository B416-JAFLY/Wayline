//! Shared sharing-card layout, also rendered by the hardware-free orb preview.
use crate::scroll_area::ScrollbarExt;
use gpui::*;

pub fn empty_share(illustration: AnyElement, action: AnyElement) -> impl IntoElement {
    page_with_actions("empty-share", illustration, action)
}

/// Scroll page contents independently from actions that must remain reachable.
pub fn page_with_actions(
    id: &'static str,
    content: AnyElement,
    actions: AnyElement,
) -> impl IntoElement {
    div()
        .id(id)
        .flex_1()
        .min_h_0()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(12.))
        .child(
            div()
                .id((id, 0_usize))
                .flex_1()
                .min_h_0()
                .visible_scrollbar()
                .flex()
                .flex_col()
                .child(content),
        )
        .child(div().flex_shrink_0().w_full().child(actions))
}

pub struct NoticeContent {
    pub id: u64,
    pub message: String,
    pub full_text: String,
    pub color: u32,
    pub expanded: bool,
    pub copied: bool,
    pub copy_label: &'static str,
    pub copied_label: &'static str,
}

pub fn notice_banner(
    content: NoticeContent,
    on_hover: impl Fn(&bool, &mut Window, &mut App) + 'static,
    on_details_hover: impl Fn(&bool, &mut Window, &mut App) + 'static,
    on_copy: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    on_dismiss: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(("share-notice", content.id))
        .flex_shrink_0()
        .w_full()
        .min_w_0()
        .rounded(px(8.))
        .bg(rgb(0x27272b))
        .flex()
        .flex_col()
        .on_hover(on_hover)
        .children(content.expanded.then(|| {
            div()
                .id("share-notice-details")
                .w_full()
                .min_w_0()
                .max_h(px(160.))
                .flex_shrink_0()
                .visible_scrollbar()
                .p(px(10.))
                .text_size(px(11.))
                .line_height(px(17.))
                .text_color(rgb(content.color))
                .whitespace_normal()
                .on_hover(on_details_hover)
                .child(content.full_text)
        }))
        .child(
            div()
                .h(px(36.))
                .flex_shrink_0()
                .w_full()
                .min_w_0()
                .px(px(6.))
                .flex()
                .items_center()
                .gap(px(6.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .line_clamp(1)
                        .text_size(px(11.))
                        .line_height(px(16.))
                        .text_color(rgb(content.color))
                        .children((!content.expanded).then_some(content.message)),
                )
                .child(
                    div()
                        .id("copy-notice")
                        .w(px(48.))
                        .h(px(24.))
                        .flex_shrink_0()
                        .rounded(px(6.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .text_size(px(10.))
                        .text_color(rgb(if content.copied { 0x30d158 } else { 0xa0a0a8 }))
                        .hover(|style| style.bg(rgb(0x38383d)))
                        .child(if content.copied {
                            content.copied_label
                        } else {
                            content.copy_label
                        })
                        .on_click(on_copy),
                )
                .child(
                    div()
                        .id("dismiss-notice")
                        .size(px(24.))
                        .flex_shrink_0()
                        .rounded(px(6.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .text_size(px(17.))
                        .text_color(rgb(0xa0a0a8))
                        .hover(|style| style.bg(rgb(0x38383d)))
                        .child("×")
                        .on_click(on_dismiss),
                ),
        )
}

pub fn notice_overlay(
    content: NoticeContent,
    position: omdrop_host::NotificationPosition,
    on_hover: impl Fn(&bool, &mut Window, &mut App) + 'static,
    on_details_hover: impl Fn(&bool, &mut Window, &mut App) + 'static,
    on_copy: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    on_dismiss: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let id = content.id;
    let banner = notice_banner(content, on_hover, on_details_hover, on_copy, on_dismiss);
    let mut overlay = div()
        .id(("share-notice-overlay", id))
        .absolute()
        .w(px(280.))
        .min_w_0()
        .child(banner);
    overlay = match position {
        omdrop_host::NotificationPosition::TopLeft => overlay.top(px(62.)).left(px(22.)),
        omdrop_host::NotificationPosition::TopCenter => overlay.top(px(62.)).left(px(38.)),
        omdrop_host::NotificationPosition::TopRight => overlay.top(px(62.)).right(px(22.)),
        omdrop_host::NotificationPosition::BottomLeft => overlay.bottom(px(104.)).left(px(22.)),
        omdrop_host::NotificationPosition::BottomCenter => overlay.bottom(px(104.)).left(px(38.)),
        omdrop_host::NotificationPosition::BottomRight => overlay.bottom(px(104.)).right(px(22.)),
    };
    overlay
}
