//! Hardware-free preview of the shared desktop scroll area.
//! Run `cargo run -p wayline-desktop --example scroll_preview --locked` on Windows.
//! Press 1/2/3 for long, short and nested content, or Esc to quit.
//! Optional --short / --nested chooses the initial page.

#[cfg(windows)]
#[path = "../src/scroll_area.rs"]
mod scroll_area;

#[cfg(windows)]
use gpui::*;
#[cfg(windows)]
use scroll_area::ScrollbarExt;

#[cfg(windows)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Long,
    Short,
    Nested,
}

#[cfg(windows)]
struct ScrollPreview {
    page: Page,
    focus: FocusHandle,
    footer_clicks: usize,
}

#[cfg(windows)]
impl ScrollPreview {
    fn select(&mut self, page: Page, cx: &mut Context<Self>) {
        self.page = page;
        cx.notify();
    }

    fn content(&self) -> AnyElement {
        match self.page {
            Page::Long | Page::Short => {
                let count = if self.page == Page::Long { 20 } else { 2 };
                let id = if self.page == Page::Long {
                    "preview-long-list"
                } else {
                    "preview-short-list"
                };
                div()
                    .id(id)
                    .flex_1()
                    .min_h_0()
                    .visible_scrollbar()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .children((1..=count).map(preview_item))
                    .into_any_element()
            }
            Page::Nested => div()
                .id("preview-nested-page")
                .flex_1()
                .min_h_0()
                .visible_scrollbar()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(12.))
                        .child("嵌套区域：滚动条分别跟随各自的内容。"),
                )
                .child(
                    div()
                        .id("preview-nested-text")
                        .h(px(92.))
                        .flex_shrink_0()
                        .rounded(px(10.))
                        .bg(rgb(0x27272b))
                        .p(px(10.))
                        .visible_scrollbar()
                        .child(
                            div()
                                .text_size(px(11.))
                                .line_height(px(17.))
                                .whitespace_normal()
                                .child(
                                    "这是长文本预览。请滚动、拖动滑块或点击轨道，确认首尾内容均可到达。\n"
                                        .repeat(12),
                                ),
                        ),
                )
                .child(
                    div()
                        .id("preview-nested-list")
                        .h(px(144.))
                        .flex_shrink_0()
                        .visible_scrollbar()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .children((1..=10).map(preview_item)),
                )
                .children((11..=18).map(preview_item))
                .into_any_element(),
        }
    }
}

#[cfg(windows)]
impl Render for ScrollPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .p(px(18.))
            .bg(rgb(0x1c1c1f))
            .text_color(rgb(0xf5f5f7))
            .font_family("Microsoft YaHei UI")
            .flex()
            .flex_col()
            .gap(px(10.))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                match event.keystroke.key.as_str() {
                    "1" => this.select(Page::Long, cx),
                    "2" => this.select(Page::Short, cx),
                    "3" => this.select(Page::Nested, cx),
                    "escape" => cx.quit(),
                    _ => {}
                }
            }))
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(px(17.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("滚动组件预览"),
            )
            .child(
                div().flex_shrink_0().flex().gap(px(6.)).children(
                    [
                        (Page::Long, "1 · 长列表"),
                        (Page::Short, "2 · 短列表"),
                        (Page::Nested, "3 · 嵌套"),
                    ]
                    .into_iter()
                    .map(|(page, label)| {
                        div()
                            .id(label)
                            .flex_1()
                            .h(px(30.))
                            .rounded(px(7.))
                            .bg(rgb(if self.page == page {
                                0x0a84ff
                            } else {
                                0x303037
                            }))
                            .text_size(px(11.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| this.select(page, cx)))
                            .child(label)
                    }),
                ),
            )
            .child(self.content())
            .child(
                div()
                    .id("preview-fixed-footer")
                    .flex_shrink_0()
                    .h(px(36.))
                    .rounded(px(10.))
                    .bg(rgb(0x303037))
                    .text_size(px(12.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .child(format!("固定操作 · 已点击 {} 次", self.footer_clicks))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.footer_clicks += 1;
                        cx.notify();
                    })),
            )
    }
}

#[cfg(windows)]
fn preview_item(index: usize) -> Div {
    div()
        .flex_shrink_0()
        .rounded(px(10.))
        .bg(rgb(0x27272b))
        .p(px(10.))
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(
            div()
                .text_size(px(12.))
                .child(format!("第 {index:02} 条记录 · iPhone")),
        )
        .child(
            div()
                .text_size(px(10.))
                .text_color(rgb(0xa0a0a8))
                .child(format!("photo-{index:02}.jpg · 2.4 MB")),
        )
}

#[cfg(windows)]
fn main() {
    let page = if std::env::args().any(|arg| arg == "--short") {
        Page::Short
    } else if std::env::args().any(|arg| arg == "--nested") {
        Page::Nested
    } else {
        Page::Long
    };
    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(360.), px(476.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Wayline Scroll Preview".into()),
                    ..Default::default()
                }),
                is_resizable: false,
                is_minimizable: false,
                ..Default::default()
            },
            |window, cx| {
                cx.new(|cx| {
                    let focus = cx.focus_handle();
                    focus.focus(window);
                    ScrollPreview {
                        page,
                        focus,
                        footer_clicks: 0,
                    }
                })
            },
        )
        .expect("open shared scroll area preview");
    });
}

#[cfg(not(windows))]
fn main() {
    eprintln!("scroll_preview requires Windows");
}
