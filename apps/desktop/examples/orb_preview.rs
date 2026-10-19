//! Hardware-free preview of the desktop connection orb and sharing-card layout.
//! Run `cargo run -p omdrop-desktop --example orb_preview --locked` on Windows.
//! Click the button or press Space to toggle connection; press Esc to quit.
//! Add --borderless to inspect the same transparent corners as the sharing card.
//! Add --notice or --long-notice to inspect the dismissible notification layout.
//! Add --expanded-notice to start with the full diagnostic details open.
//! Add --long-content to stress the same scrolling content / pinned action layout as firmware.
//! Add --check-layout to validate the 356 x 476 layout and exit without interaction.

#[cfg(windows)]
#[path = "../src/brand.rs"]
mod brand;
#[cfg(windows)]
#[path = "../src/orb.rs"]
mod orb;
#[cfg(windows)]
#[path = "../src/orb_geometry.rs"]
mod orb_geometry;
#[cfg(windows)]
#[path = "../src/share_layout.rs"]
mod share_layout;
#[cfg(windows)]
#[path = "../src/window_frame.rs"]
mod window_frame;

#[cfg(windows)]
use gpui::*;
#[cfg(windows)]
use std::{
    cell::Cell,
    rc::Rc,
    time::{Duration, Instant},
};

#[cfg(windows)]
struct PreviewNotice {
    message: String,
    full_text: String,
}

#[cfg(windows)]
struct OrbPreview {
    orb: orb::ConnectionOrb,
    connected: bool,
    focus: FocusHandle,
    notice: Option<PreviewNotice>,
    notice_expanded: bool,
    notice_copied: bool,
    notice_hovered: bool,
    details_hovered: bool,
    hide_notice: Option<Task<()>>,
    check_layout: bool,
    layout_checked: Rc<Cell<bool>>,
    check_started: Instant,
    long_content: bool,
}

#[cfg(windows)]
impl OrbPreview {
    fn toggle(&mut self, cx: &mut Context<Self>) {
        self.connected = !self.connected;
        cx.notify();
    }

    fn hover_notice(&mut self, details: bool, hovered: bool, cx: &mut Context<Self>) {
        // Automated layout checks keep their requested initial state and do not
        // depend on the physical pointer's position over this preview window.
        if self.check_layout {
            return;
        }
        if details {
            self.details_hovered = hovered;
        } else {
            self.notice_hovered = hovered;
        }
        if self.notice_hovered || self.details_hovered {
            self.hide_notice = None;
            if !self.notice_expanded {
                self.notice_expanded = true;
                cx.notify();
            }
        } else if self.notice_expanded && self.hide_notice.is_none() {
            self.hide_notice = Some(cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(350))
                    .await;
                let _ = this.update(cx, |this, cx| {
                    this.hide_notice = None;
                    if !this.notice_hovered && !this.details_hovered {
                        this.notice_expanded = false;
                        cx.notify();
                    }
                });
            }));
        }
    }
}

#[cfg(windows)]
impl Render for OrbPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.orb.update(true, self.connected);
        let action_bounds = Rc::new(Cell::new(None::<Bounds<Pixels>>));
        let measured_action = Rc::clone(&action_bounds);
        let check_layout = self.check_layout;
        let layout_checked = Rc::clone(&self.layout_checked);
        let check_started = self.check_started;
        let has_notice = self.notice.is_some();
        let notice_expanded = self.notice_expanded;
        div()
            .on_children_prepainted(move |children, window, cx| {
                if !check_layout || layout_checked.get() {
                    return;
                }
                let viewport = Bounds::new(Point::default(), window.viewport_size());
                // Borderless sizing is posted to the window after creation. Wait
                // for that size to reach GPUI before validating the actual card.
                if !near(viewport.size.width, px(356.))
                    || !near(viewport.size.height, px(476.))
                {
                    assert!(
                        check_started.elapsed().as_secs() < 5,
                        "layout check did not reach a 356 x 476 viewport: {viewport:?}"
                    );
                    window.request_animation_frame();
                    return;
                }
                assert_eq!(children.len(), if has_notice { 4 } else { 3 });
                let header = children[0];
                let content = children[1];
                let footer = *children.last().unwrap();
                let action = action_bounds
                    .get()
                    .expect("action canvas must be prepainted before its parent");
                assert!(near(action.size.height, px(42.)), "action: {action:?}");
                assert!(inside(action, content), "action {action:?}, content {content:?}");
                for (name, bounds) in [("header", header), ("content", content), ("action", action), ("footer", footer)] {
                    assert!(inside(bounds, viewport), "{name} {bounds:?}, viewport {viewport:?}");
                }
                assert!(header.bottom() <= content.top(), "header overlaps content");
                assert!(content.bottom() <= footer.top(), "content overlaps footer");
                if has_notice {
                    let notice = children[2];
                    let expected_height = if notice_expanded { 196. } else { 36. };
                    assert!(near(notice.size.height, px(expected_height)), "notice: {notice:?}");
                    assert!(inside(notice, viewport), "notice: {notice:?}");
                    assert!(!notice.intersects(&action), "notice {notice:?}, action {action:?}");
                    assert!(content.bottom() <= notice.top(), "notice overlaps content");
                    assert!(notice.bottom() <= footer.top(), "notice overlaps footer");
                    println!("layout notice: {notice:?}");
                }
                println!("layout viewport: {viewport:?}; action: {action:?}; content: {content:?}; footer: {footer:?}");
                println!("layout check passed");
                layout_checked.set(true);
                cx.defer(|cx| cx.quit());
            })
            .id("orb-preview")
            .size_full()
            .rounded(px(window_frame::CORNER_RADIUS))
            .bg(rgb(0x1c1c1f))
            .border_1()
            .border_color(rgba(0xffffff24))
            .p(px(22.))
            .overflow_hidden()
            .flex()
            .flex_col()
            .font_family("Segoe UI")
            .text_color(rgb(0xf5f5f7))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                match event.keystroke.key.as_str() {
                    "space" => this.toggle(cx),
                    "escape" => cx.quit(),
                    _ => {}
                }
            }))
            .child(
                div()
                    .h(px(28.))
                    .mb(px(18.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(9.))
                            .child(brand::mark(25.))
                            .child(
                                div()
                                    .text_size(px(17.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Omdrop"),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(rgb(0xa0a0a8))
                            .child("动效预览"),
                    ),
            )
            .child(share_layout::empty_share(
                div()
                    .my_auto()
                    .w_full()
                    .flex_shrink_0()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(12.))
                    .child(self.orb.element())
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(23.))
                            .line_height(px(30.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("轻轻一拖，即刻分享"),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(12.))
                            .line_height(px(20.))
                            .text_color(rgb(0xa0a0a8))
                            .text_center()
                            .child("将文件拖到这里\n发送到附近的 iPhone 或 Mac"),
                    )
                    .children(self.long_content.then(|| {
                        div()
                            .w_full()
                            .text_size(px(11.))
                            .line_height(px(16.))
                            .whitespace_normal()
                            .child("长页面滚动检查：说明内容应当滚动，操作按钮始终可见。\n".repeat(40))
                    }))
                    .into_any_element(),
                div()
                    .id("toggle-connection")
                    .relative()
                    .mt(px(8.))
                    .h(px(42.))
                    .w_full()
                    .flex_shrink_0()
                    .rounded(px(13.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .text_size(px(13.))
                    .font_weight(FontWeight::MEDIUM)
                    .bg(rgb(0x0a84ff))
                    .text_color(rgb(0xffffff))
                    .hover(|style| style.bg(rgb(0x2592ff)))
                    .child("切换连接状态")
                    .children(check_layout.then(|| {
                        canvas(
                            move |bounds, _, _| measured_action.set(Some(bounds)),
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                    }))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle(cx)))
                    .into_any_element(),
            ))
            .children(self.notice.as_ref().map(|notice| {
                share_layout::notice_banner(
                    share_layout::NoticeContent {
                        id: 1,
                        message: notice.message.clone(),
                        full_text: notice.full_text.clone(),
                        color: 0xff9f0a,
                        expanded: self.notice_expanded,
                        copied: self.notice_copied,
                    },
                    cx.listener(|this, hovered, _, cx| {
                        this.hover_notice(false, *hovered, cx);
                    }),
                    cx.listener(|this, hovered, _, cx| {
                        this.hover_notice(true, *hovered, cx);
                    }),
                    cx.listener(|this, _, _, cx| {
                        if !this.check_layout
                            && let Some(notice) = &this.notice
                        {
                            cx.write_to_clipboard(ClipboardItem::new_string(notice.full_text.clone()));
                            this.notice_copied = true;
                            cx.notify();
                        }
                    }),
                    cx.listener(|this, _, _, cx| {
                        this.notice = None;
                        this.notice_expanded = false;
                        this.notice_copied = false;
                        this.notice_hovered = false;
                        this.details_hovered = false;
                        this.hide_notice = None;
                        cx.notify();
                    }),
                )
            }))
            .child(
                div()
                    .mt(px(18.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_between()
                    .text_size(px(10.))
                    .text_color(rgb(0xa0a0a8))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(div().size(px(5.)).rounded_full().bg(rgb(if self.connected {
                                0x30d158
                            } else {
                                0xff9f0a
                            })))
                            .child(if self.connected {
                                "已连接（模拟）"
                            } else {
                                "未连接（模拟）"
                            }),
                    )
                    .child("Space 切换 · Esc 退出"),
            )
    }
}

#[cfg(windows)]
fn near(actual: Pixels, expected: Pixels) -> bool {
    f32::from(actual - expected).abs() <= 0.5
}

#[cfg(windows)]
fn inside(inner: Bounds<Pixels>, outer: Bounds<Pixels>) -> bool {
    let tolerance = px(0.5);
    inner.left() >= outer.left() - tolerance
        && inner.top() >= outer.top() - tolerance
        && inner.right() <= outer.right() + tolerance
        && inner.bottom() <= outer.bottom() + tolerance
}

#[cfg(windows)]
fn main() {
    let check_layout = std::env::args().any(|arg| arg == "--check-layout");
    let long_content = std::env::args().any(|arg| arg == "--long-content");
    let borderless = check_layout || std::env::args().any(|arg| arg == "--borderless");
    let notice_expanded = std::env::args().any(|arg| arg == "--expanded-notice");
    let notice = if notice_expanded || std::env::args().any(|arg| arg == "--long-notice") {
        let message = "接收未完成：发送方连接中断，请重新发送。诊断：stage=HTTP body completion，received=6291456，total=unknown，content_type=application/x-dvzip，HTTP stage=ChunkHeader，chunk_remaining=0，socket idle timeout。".to_owned();
        let full_text = format!(
            "{message}\n\n完整诊断（模拟）：\n{}",
            "HTTP body completion: received=6291456, total=unknown, content_type=application/x-dvzip, stage=ChunkHeader, chunk_remaining=0, socket idle timeout.\n".repeat(8)
        );
        Some(PreviewNotice { message, full_text })
    } else if std::env::args().any(|arg| arg == "--notice") {
        let message = "接收未完成：发送方连接中断，请重新发送。".to_owned();
        Some(PreviewNotice {
            full_text: message.clone(),
            message,
        })
    } else {
        None
    };
    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(356.), px(476.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Omdrop · 球形点阵预览".into()),
                    ..Default::default()
                }),
                is_resizable: false,
                is_minimizable: false,
                show: !borderless,
                window_background: WindowBackgroundAppearance::Transparent,
                ..Default::default()
            },
            |window, cx| {
                if borderless {
                    prepare_borderless(window);
                }
                cx.new(|cx| {
                    let focus = cx.focus_handle();
                    focus.focus(window);
                    OrbPreview {
                        orb: orb::ConnectionOrb::default(),
                        connected: true,
                        focus,
                        notice,
                        notice_expanded,
                        notice_copied: false,
                        notice_hovered: false,
                        details_hovered: false,
                        hide_notice: None,
                        check_layout,
                        long_content,
                        layout_checked: Rc::new(Cell::new(false)),
                        check_started: Instant::now(),
                    }
                })
            },
        )
        .expect("open connection orb preview");
    });
}

#[cfg(windows)]
fn prepare_borderless(window: &Window) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::{
        Foundation::*,
        UI::{Shell::SetWindowSubclass, WindowsAndMessaging::*},
    };
    let RawWindowHandle::Win32(handle) = HasWindowHandle::window_handle(window).unwrap().as_raw()
    else {
        unreachable!("Windows preview");
    };
    let hwnd = HWND(handle.hwnd.get() as *mut _);
    // SAFETY: this preview owns the live window and installs the callback on its
    // UI thread. The posted message delays size changes until GPUI releases App.
    unsafe {
        SetWindowSubclass(hwnd, Some(preview_frame), 1, 0)
            .ok()
            .unwrap();
        PostMessageW(Some(hwnd), WM_APP + 70, WPARAM(0), LPARAM(0)).unwrap();
    }
}

#[cfg(windows)]
unsafe extern "system" fn preview_frame(
    hwnd: windows::Win32::Foundation::HWND,
    message: u32,
    wp: windows::Win32::Foundation::WPARAM,
    lp: windows::Win32::Foundation::LPARAM,
    _: usize,
    _: usize,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::{
        Foundation::*,
        UI::{HiDpi::*, Shell::DefSubclassProc, WindowsAndMessaging::*},
    };
    // SAFETY: Windows calls this for the preview's HWND on its owning thread;
    // no borrowed memory or cross-thread GPUI state is accessed.
    unsafe {
        if message == WM_NCCALCSIZE {
            return LRESULT(0);
        }
        if message == WM_NCHITTEST && window_frame::transparent_corner(hwnd, lp) {
            return LRESULT(HTTRANSPARENT as isize);
        }
        if message == WM_APP + 70 {
            let scale = GetDpiForWindow(hwnd).max(96) as f32 / 96.;
            let width = (356. * scale).round() as i32;
            let height = (476. * scale).round() as i32;
            window_frame::configure(hwnd, width, height);
            let _ = SetWindowPos(
                hwnd,
                None,
                0,
                0,
                width,
                height,
                SWP_NOMOVE | SWP_NOZORDER | SWP_FRAMECHANGED,
            );
            let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
            let _ = SetForegroundWindow(hwnd);
            return LRESULT(0);
        }
        DefSubclassProc(hwnd, message, wp, lp)
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("orb_preview requires Windows");
}
