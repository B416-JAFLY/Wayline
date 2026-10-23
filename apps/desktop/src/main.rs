#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(windows)]
mod backend;
#[cfg(windows)]
mod brand;
#[cfg(windows)]
mod file_drop;
#[cfg(windows)]
mod firmware;
#[cfg(windows)]
mod i18n;
#[cfg(windows)]
mod integration;
#[cfg(windows)]
mod model;
#[cfg(windows)]
mod name_input;
#[cfg(windows)]
mod native;
#[cfg(windows)]
mod orb;
#[cfg(windows)]
mod orb_geometry;
#[cfg(windows)]
mod picker;
#[cfg(windows)]
mod preview;
#[cfg(windows)]
mod receiving;
#[cfg(windows)]
mod settings;
#[cfg(windows)]
mod share_layout;
#[cfg(windows)]
mod ui;
#[cfg(windows)]
mod window_frame;

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use gpui::*;
    if std::env::args_os().len() == 2
        && std::env::args_os()
            .nth(1)
            .is_some_and(|arg| arg == "--version")
    {
        println!("omdrop-desktop {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let launch = match integration::Mode::parse(std::env::args_os().skip(1))? {
        integration::Mode::Service => {
            // Session 0 must never initialize GPUI, acquire the interactive
            // singleton, or register Explorer entries for the SYSTEM account.
            return Ok(omdrop_service::service::dispatch()?);
        }
        integration::Mode::Desktop(launch) => launch,
    };
    omdrop_core::enable_board_feedback();
    if launch.unregister {
        return integration::unregister();
    }
    let Some(_instance) = native::Instance::acquire(&launch.files)? else {
        return Ok(());
    };
    Application::new().run(move |cx: &mut App| {
        name_input::init(cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.), px(0.)),
                    size(px(200.), px(56.)),
                ))),
                titlebar: None,
                kind: WindowKind::PopUp,
                focus: false,
                show: false,
                is_movable: false,
                is_resizable: false,
                is_minimizable: false,
                window_background: WindowBackgroundAppearance::Transparent,
                ..Default::default()
            },
            |window, cx| cx.new(|cx| ui::Capsule::new(window, cx, launch.files)),
        )
        .expect("open Omdrop capsule");
    });
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("omdrop-desktop requires Windows");
}
