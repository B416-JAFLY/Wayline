//! Small Windows shell adapter. Networking and rendering remain in Rust/GPUI.
use anyhow::{Context, Result, bail};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{
    mem::size_of,
    os::windows::ffi::OsStrExt,
    path::PathBuf,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
    },
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        System::{
            DataExchange::COPYDATASTRUCT, LibraryLoader::GetModuleHandleW, Threading::CreateMutexW,
        },
        UI::{
            HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi},
            Input::KeyboardAndMouse::*,
            Shell::*,
            WindowsAndMessaging::*,
        },
    },
    core::{HSTRING, PCWSTR, w},
};

pub enum Action {
    Toggle,
    Show,
    Close,
    Choose,
    Files(Vec<PathBuf>),
    FilesDropped(Vec<PathBuf>),
    DropHover(bool),
    FilesPicked(Result<Option<Vec<PathBuf>>, String>),
    DownloadDirectoryChosen(Result<Option<PathBuf>, String>),
    FilesOpened(u64, Result<(), String>),
    FileDrag(bool),
    ShellError(String),
    Quit,
    Reposition,
    RestoreTray,
}
static ACTIONS: OnceLock<Sender<Action>> = OnceLock::new();
static TASKBAR_CREATED: OnceLock<u32> = OnceLock::new();
const TRAY_MESSAGE: u32 = WM_APP + 41;
const SHOW_MESSAGE: u32 = WM_APP + 42;
const PLACE_MESSAGE: u32 = WM_APP + 43;

pub struct Instance(HANDLE);
impl Instance {
    pub fn acquire(paths: &[PathBuf]) -> Result<Option<Self>> {
        let handle = unsafe { CreateMutexW(None, false, w!("Local\\OmdropDesktop"))? };
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            let _handle = Self(handle);
            let bytes = crate::integration::encode_paths(paths)?;
            // Another Explorer invocation may arrive while the first process is
            // still creating its GPU window. Wait for the subclass to be ready.
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                unsafe {
                    if let Ok(hwnd) = FindWindowW(None, w!("Omdrop"))
                        && !GetPropW(hwnd, w!("OmdropDesktop.Ready")).is_invalid()
                    {
                        let mut pid = 0;
                        GetWindowThreadProcessId(hwnd, Some(&mut pid));
                        let _ = AllowSetForegroundWindow(pid);
                        let data = COPYDATASTRUCT {
                            dwData: crate::integration::COPY_DATA_ID,
                            cbData: bytes.len() as u32,
                            lpData: bytes.as_ptr() as *mut _,
                        };
                        let mut accepted = 0;
                        SendMessageTimeoutW(
                            hwnd,
                            WM_COPYDATA,
                            WPARAM(0),
                            LPARAM(&data as *const _ as isize),
                            SMTO_ABORTIFHUNG | SMTO_BLOCK,
                            2000,
                            Some(&mut accepted),
                        );
                        if accepted == 1 {
                            return Ok(None);
                        }
                        bail!("Omdrop 未能接收文件，请稍后重试");
                    }
                }
                if Instant::now() >= deadline {
                    bail!("等待 Omdrop 窗口超时");
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        Ok(Some(Self(handle)))
    }
}
impl Drop for Instance {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

struct Icon(HICON);
impl Icon {
    fn load(resource: u16, width: i32, height: i32) -> Result<Self> {
        // SAFETY: the current executable remains loaded for the process lifetime.
        // The small integer pointer is Win32's MAKEINTRESOURCE representation,
        // not memory to dereference. Omitting LR_SHARED gives this wrapper unique
        // ownership and preserves distinct requested sizes for the same resource.
        let handle = unsafe {
            let module = GetModuleHandleW(None)?;
            LoadImageW(
                Some(HINSTANCE(module.0)),
                PCWSTR(usize::from(resource) as *const u16),
                IMAGE_ICON,
                width,
                height,
                LR_DEFAULTCOLOR,
            )?
        };
        Ok(Self(HICON(handle.0)))
    }
}
impl Drop for Icon {
    fn drop(&mut self) {
        // SAFETY: LoadImageW without LR_SHARED returned this owned icon. Shell
        // detaches its tray and window references before its Icon fields drop.
        unsafe {
            let _ = DestroyIcon(self.0);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TrayUpdate {
    Add,
    Modify,
}

#[derive(Default)]
struct TrayState {
    connected: bool,
    applied: Option<bool>,
    dirty: bool,
}
impl TrayState {
    fn pending(&self) -> Option<TrayUpdate> {
        match self.applied {
            None => Some(TrayUpdate::Add),
            Some(connected) if self.dirty || connected != self.connected => {
                Some(TrayUpdate::Modify)
            }
            Some(_) => None,
        }
    }

    fn applied(&mut self, success: bool) {
        if success {
            self.applied = Some(self.connected);
            self.dirty = false;
        }
    }
}

pub struct Shell {
    hwnd: HWND,
    window_small_icon: Icon,
    window_big_icon: Icon,
    previous_small_icon: HICON,
    previous_big_icon: HICON,
    connected_icon: Icon,
    disconnected_icon: Icon,
    tray: TrayState,
    tray_error_reported: bool,
    stop_drag_monitor: Arc<AtomicBool>,
}
impl Shell {
    pub fn new(window: &gpui::Window, tx: Sender<Action>) -> Result<Self> {
        let hwnd = hwnd(window).context("获取桌面窗口句柄失败")?;
        // SAFETY: construction runs on this HWND's GPUI thread; these APIs only
        // read DPI/metrics. Load all owned icons before registering shell hooks.
        let (small_width, small_height, big_width, big_height) = unsafe {
            let dpi = GetDpiForWindow(hwnd).max(96);
            (
                GetSystemMetricsForDpi(SM_CXSMICON, dpi).max(16),
                GetSystemMetricsForDpi(SM_CYSMICON, dpi).max(16),
                GetSystemMetricsForDpi(SM_CXICON, dpi).max(32),
                GetSystemMetricsForDpi(SM_CYICON, dpi).max(32),
            )
        };
        let window_small_icon =
            Icon::load(1, small_width, small_height).context("加载应用小图标失败")?;
        let window_big_icon = Icon::load(1, big_width, big_height).context("加载应用大图标失败")?;
        let connected_icon =
            Icon::load(2, small_width, small_height).context("加载已连接托盘图标失败")?;
        let disconnected_icon =
            Icon::load(3, small_width, small_height).context("加载未连接托盘图标失败")?;
        // SAFETY: the registered message name is a static UTF-16 string. The
        // subclass only queues an Action; it never borrows the GPUI application.
        let taskbar_created = *TASKBAR_CREATED
            .get_or_init(|| unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) });
        if taskbar_created == 0 {
            bail!("注册托盘恢复消息失败");
        }
        let _ = ACTIONS.set(tx.clone());
        let taskbar_mode = std::env::args_os().any(|arg| arg == "--show-in-taskbar");
        unsafe {
            SetWindowTextW(hwnd, w!("Omdrop")).context("设置桌面窗口标题失败")?;
            // Optional taskbar presence also makes this utility reachable to
            // desktop accessibility/inspection tools that omit tool windows.
            if taskbar_mode {
                let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
                SetWindowLongPtrW(
                    hwnd,
                    GWL_EXSTYLE,
                    ((style & !WS_EX_TOOLWINDOW.0) | WS_EX_APPWINDOW.0) as isize,
                );
            }
            SetWindowSubclass(hwnd, Some(subclass), 1, 0)
                .ok()
                .context("注册桌面窗口消息处理失败")?;
            crate::file_drop::register(hwnd, tx.clone()).context("注册资源管理器拖放失败")?;
            SetPropW(
                hwnd,
                w!("OmdropDesktop.Ready"),
                Some(HANDLE(std::ptr::dangling_mut())),
            )
            .context("注册桌面窗口标记失败")?;
        }
        let stop_drag_monitor = Arc::new(AtomicBool::new(false));
        let mut shell = Self {
            hwnd,
            window_small_icon,
            window_big_icon,
            previous_small_icon: HICON::default(),
            previous_big_icon: HICON::default(),
            connected_icon,
            disconnected_icon,
            tray: TrayState::default(),
            tray_error_reported: false,
            stop_drag_monitor: stop_drag_monitor.clone(),
        };
        // SAFETY: Shell owns both icons and lives on the window's GPUI thread.
        // GPUI 0.2.2 sends WM_SETICON straight to DefWindowProcW, so this does not
        // invoke its App callbacks. Restore the borrowed old handles before Drop.
        unsafe {
            shell.previous_small_icon = HICON(
                SendMessageW(
                    hwnd,
                    WM_SETICON,
                    Some(WPARAM(ICON_SMALL as usize)),
                    Some(LPARAM(shell.window_small_icon.0.0 as isize)),
                )
                .0 as *mut _,
            );
            shell.previous_big_icon = HICON(
                SendMessageW(
                    hwnd,
                    WM_SETICON,
                    Some(WPARAM(ICON_BIG as usize)),
                    Some(LPARAM(shell.window_big_icon.0.0 as isize)),
                )
                .0 as *mut _,
            );
        }
        if !shell.apply_tray() {
            // Shell_NotifyIcon only documents a BOOL result, not GetLastError.
            // An explicitly visible taskbar window remains usable without a tray.
            if !taskbar_mode {
                bail!(crate::i18n::current().strings().err_tray_add);
            }
            shell.tray_error_reported = true;
            let _ = tx.send(Action::ShellError(
                crate::i18n::current().strings().err_tray_hidden.into(),
            ));
        }
        monitor_file_drag(tx.clone(), stop_drag_monitor);
        std::thread::spawn(move || {
            if let Err(error) = crate::integration::register() {
                let _ = tx.send(Action::ShellError(format!(
                    "{}{error}",
                    crate::i18n::current().strings().shell_menu_failed
                )));
            }
        });
        Ok(shell)
    }

    fn tray_data(&self) -> NOTIFYICONDATAW {
        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: 1,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage: TRAY_MESSAGE,
            hIcon: if self.tray.connected {
                self.connected_icon.0
            } else {
                self.disconnected_icon.0
            },
            ..Default::default()
        };
        let text = crate::i18n::current().strings();
        for (out, value) in data.szTip.iter_mut().zip(
            if self.tray.connected {
                text.tray_connected
            } else {
                text.tray_disconnected
            }
            .encode_utf16(),
        ) {
            *out = value;
        }
        data
    }

    pub fn set_connected(&mut self, connected: bool) {
        self.tray.connected = connected;
        self.sync_tray();
    }

    /// Refresh the tooltip after the interface language changes.
    pub fn language_changed(&mut self) {
        self.tray.dirty = true;
        self.sync_tray();
    }

    pub fn restore_tray(&mut self) {
        self.tray.applied = None;
        self.sync_tray();
    }

    fn sync_tray(&mut self) {
        if !self.apply_tray() && !self.tray_error_reported {
            self.tray_error_reported = true;
            if let Some(tx) = ACTIONS.get() {
                let _ = tx.send(Action::ShellError(
                    crate::i18n::current().strings().err_tray_update.into(),
                ));
            }
        }
    }

    fn apply_tray(&mut self) -> bool {
        let Some(update) = self.tray.pending() else {
            return true;
        };
        let mut data = self.tray_data();
        let (operation, fallback) = match update {
            TrayUpdate::Add => (NIM_ADD, NIM_MODIFY),
            TrayUpdate::Modify => {
                data.uFlags = NIF_ICON | NIF_TIP;
                (NIM_MODIFY, NIM_ADD)
            }
        };
        // SAFETY: the complete stack-allocated structure remains valid through
        // each synchronous call; cached icons remain owned by this UI-thread Shell.
        let mut success = unsafe { Shell_NotifyIconW(operation, &data) }.as_bool();
        if !success {
            // Recover from an Explorer restart even if its broadcast was missed,
            // or from an ADD retry after Explorer already installed the icon.
            data = self.tray_data();
            success = unsafe { Shell_NotifyIconW(fallback, &data) }.as_bool();
        }
        self.tray.applied(success);
        if success {
            self.tray_error_reported = false;
        }
        success
    }

    pub fn notify(&self, title: &str, message: &str) {
        let mut data = self.tray_data();
        data.uFlags = NIF_INFO;
        data.dwInfoFlags = NIIF_INFO;
        for (out, value) in data
            .szInfoTitle
            .iter_mut()
            .take(63)
            .zip(title.encode_utf16())
        {
            *out = value;
        }
        for (out, value) in data.szInfo.iter_mut().take(255).zip(message.encode_utf16()) {
            *out = value;
        }
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
        }
    }

    pub fn choose_files(&self) -> Result<()> {
        let owner = self.hwnd.0 as usize;
        let tx = ACTIONS.get().unwrap().clone();
        std::thread::Builder::new()
            .name("omdrop-file-picker".into())
            .spawn(move || {
                let language = crate::i18n::current();
                let result =
                    crate::picker::choose(owner, language).map_err(|error| error.to_string());
                let _ = tx.send(Action::FilesPicked(result));
            })?;
        Ok(())
    }

    pub fn choose_download_directory(&self) -> Result<()> {
        let owner = self.hwnd.0 as usize;
        let tx = ACTIONS.get().unwrap().clone();
        std::thread::Builder::new()
            .name("omdrop-directory-picker".into())
            .spawn(move || {
                let language = crate::i18n::current();
                let result = crate::picker::choose_directory(owner, language).map_err(|error| {
                    format!("{}{error}", language.strings().err_receive_dir_picker)
                });
                let _ = tx.send(Action::DownloadDirectoryChosen(result));
            })
            .context("启动接收保存目录选择器失败")?;
        Ok(())
    }

    pub fn open_files(&self, selection_id: u64, paths: Vec<PathBuf>) -> Result<()> {
        let owner = self.hwnd.0 as usize;
        let tx = ACTIONS.get().unwrap().clone();
        std::thread::Builder::new()
            .name("omdrop-open-files".into())
            .spawn(move || {
                let result = (|| -> Result<()> {
                    let _apartment = crate::picker::Apartment::new()?;
                    for path in paths {
                        let file: Vec<u16> =
                            path.as_os_str().encode_wide().chain(Some(0)).collect();
                        let mut info = SHELLEXECUTEINFOW {
                            cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
                            // This worker exits after dispatch; finish shell activation first.
                            fMask: SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
                            hwnd: HWND(owner as *mut _),
                            lpVerb: w!("open"),
                            lpFile: PCWSTR(file.as_ptr()),
                            nShow: SW_SHOWNORMAL.0,
                            ..Default::default()
                        };
                        unsafe { ShellExecuteExW(&mut info) }.with_context(|| {
                            crate::i18n::current().cannot_open_file(
                                &path.file_name().unwrap_or_default().to_string_lossy(),
                            )
                        })?;
                    }
                    Ok(())
                })()
                .map_err(|error| format!("{error:#}"));
                let _ = tx.send(Action::FilesOpened(selection_id, result));
            })?;
        Ok(())
    }

    pub fn place(&self, expanded: bool, visible: bool, activate: bool) {
        // Do not synchronously dispatch WM_SIZE while GPUI holds its App borrow.
        unsafe {
            let _ = PostMessageW(
                Some(self.hwnd),
                PLACE_MESSAGE,
                WPARAM(
                    usize::from(expanded)
                        | (usize::from(visible) << 1)
                        | (usize::from(activate) << 2),
                ),
                LPARAM(0),
            );
        }
    }
    fn position(hwnd: HWND, expanded: bool, visible: bool, activate: bool) {
        unsafe {
            if !visible {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            let mut info = MONITORINFO {
                cbSize: size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if !GetMonitorInfoW(monitor, &mut info).as_bool() {
                return;
            }
            let scale = GetDpiForWindow(hwnd).max(96) as f32 / 96.0;
            let (width, height) = if expanded {
                (356.0, 476.0)
            } else {
                (200.0, 56.0)
            };
            let (width, height, margin) = (
                (width * scale).round() as i32,
                (height * scale).round() as i32,
                (12.0 * scale).round() as i32,
            );
            crate::window_frame::configure(hwnd, width, height);
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                (info.rcWork.right - width - margin).max(info.rcWork.left),
                (info.rcWork.bottom - height - margin).max(info.rcWork.top),
                width,
                height,
                SWP_NOACTIVATE | SWP_FRAMECHANGED,
            );
            if visible && !IsWindowVisible(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            }
            if visible && activate {
                let _ = SetForegroundWindow(hwnd);
            }
        }
    }
}
impl Drop for Shell {
    fn drop(&mut self) {
        self.stop_drag_monitor.store(true, Ordering::Relaxed);
        crate::file_drop::unregister(self.hwnd);
        unsafe {
            let _ = RemovePropW(self.hwnd, w!("OmdropDesktop.Ready"));
            let _ = Shell_NotifyIconW(NIM_DELETE, &self.tray_data());
            let _ = RemoveWindowSubclass(self.hwnd, Some(subclass), 1);
            // Remove the HWND's references before the owned Icon fields drop.
            // Previous handles are borrowed from GPUI/Windows, never destroyed.
            if IsWindow(Some(self.hwnd)).as_bool() {
                let _ = SendMessageW(
                    self.hwnd,
                    WM_SETICON,
                    Some(WPARAM(ICON_SMALL as usize)),
                    Some(LPARAM(self.previous_small_icon.0 as isize)),
                );
                let _ = SendMessageW(
                    self.hwnd,
                    WM_SETICON,
                    Some(WPARAM(ICON_BIG as usize)),
                    Some(LPARAM(self.previous_big_icon.0 as isize)),
                );
            }
        }
    }
}

fn hwnd(window: &gpui::Window) -> Result<HWND> {
    match HasWindowHandle::window_handle(window)
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .as_raw()
    {
        RawWindowHandle::Win32(handle) => Ok(HWND(handle.hwnd.get() as *mut _)),
        _ => bail!("Windows window handle unavailable"),
    }
}

unsafe extern "system" fn subclass(
    hwnd: HWND,
    message: u32,
    wp: WPARAM,
    lp: LPARAM,
    _: usize,
    _: usize,
) -> LRESULT {
    if TASKBAR_CREATED
        .get()
        .is_some_and(|registered| message == *registered)
    {
        if let Some(tx) = ACTIONS.get() {
            let _ = tx.send(Action::RestoreTray);
        }
        return LRESULT(0);
    }
    if message == WM_NCCALCSIZE {
        // GPUI reserves resize-frame insets even for its PopUp HWND. This
        // non-resizable window uses the entire rect as client area; leaving
        // those insets creates the dark rectangular rim outside our surface.
        return LRESULT(0);
    }
    if message == WM_NCHITTEST && crate::window_frame::transparent_corner(hwnd, lp) {
        return LRESULT(HTTRANSPARENT as isize);
    }
    if message == PLACE_MESSAGE {
        Shell::position(hwnd, wp.0 & 1 != 0, wp.0 & 2 != 0, wp.0 & 4 != 0);
        return LRESULT(0);
    }
    if message == WM_CLOSE {
        if let Some(tx) = ACTIONS.get() {
            let _ = tx.send(Action::Close);
        }
        return LRESULT(0);
    }
    if message == WM_COPYDATA {
        if lp.0 == 0 {
            return LRESULT(0);
        }
        let data = unsafe { &*(lp.0 as *const COPYDATASTRUCT) };
        if data.dwData != crate::integration::COPY_DATA_ID
            || data.lpData.is_null()
            || data.cbData as usize > crate::integration::MAX_ACTIVATION_BYTES
        {
            return LRESULT(0);
        }
        let bytes =
            unsafe { std::slice::from_raw_parts(data.lpData.cast::<u8>(), data.cbData as usize) };
        if let Ok(paths) = crate::integration::decode_paths(bytes)
            && let Some(tx) = ACTIONS.get()
        {
            let action = if paths.is_empty() {
                Action::Show
            } else {
                Action::Files(paths)
            };
            return LRESULT(isize::from(tx.send(action).is_ok()));
        }
        return LRESULT(0);
    }
    let action = match message {
        SHOW_MESSAGE => Some(Action::Show),
        WM_DISPLAYCHANGE | WM_SETTINGCHANGE | WM_DPICHANGED => Some(Action::Reposition),
        TRAY_MESSAGE if lp.0 as u32 == WM_LBUTTONUP => Some(Action::Toggle),
        TRAY_MESSAGE if lp.0 as u32 == WM_RBUTTONUP => unsafe { tray_menu(hwnd) },
        _ => None,
    };
    if let Some(action) = action
        && let Some(tx) = ACTIONS.get()
    {
        let _ = tx.send(action);
    }
    unsafe { DefSubclassProc(hwnd, message, wp, lp) }
}

unsafe fn tray_menu(hwnd: HWND) -> Option<Action> {
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        let text = crate::i18n::current().strings();
        let toggle = HSTRING::from(text.tray_toggle);
        let choose = HSTRING::from(text.tray_choose);
        let quit = HSTRING::from(text.tray_quit);
        let _ = AppendMenuW(menu, MF_STRING, 1, &toggle);
        let _ = AppendMenuW(menu, MF_STRING, 2, &choose);
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, 3, &quit);
        let mut point = POINT::default();
        let _ = GetCursorPos(&mut point);
        let _ = SetForegroundWindow(hwnd);
        let selected = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
            point.x,
            point.y,
            None,
            hwnd,
            None,
        )
        .0;
        let _ = DestroyMenu(menu);
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        match selected {
            1 => Some(Action::Toggle),
            2 => Some(Action::Choose),
            3 => Some(Action::Quit),
            _ => None,
        }
    }
}

// Explorer and the desktop create a Shell drag-image window only for an item
// drag. Mouse-down/movement alone must not reveal the capsule (text selection,
// resizing and moving windows are not file drags). Once detected, keep the
// target stable until release/Escape, including while Explorer hides its image
// over a drop target. No global mouse hook or invisible input-blocking window.
fn monitor_file_drag(tx: Sender<Action>, stop: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let mut active = false;
        let mut cancelled = false;
        let mut ended = None;
        while !stop.load(Ordering::Relaxed) {
            let held = unsafe {
                GetAsyncKeyState(VK_LBUTTON.0 as i32) < 0
                    || GetAsyncKeyState(VK_RBUTTON.0 as i32) < 0
            };
            let escape = unsafe { GetAsyncKeyState(VK_ESCAPE.0 as i32) < 0 };
            if !held {
                cancelled = false;
            }
            if escape {
                cancelled = true;
            }
            if active && (!held || cancelled) {
                // Let the OLE drop callback enqueue the files before hiding.
                let deadline =
                    ended.get_or_insert_with(|| Instant::now() + Duration::from_millis(160));
                if Instant::now() >= *deadline {
                    active = false;
                    ended = None;
                    if tx.send(Action::FileDrag(false)).is_err() {
                        break;
                    }
                }
            } else if active {
                ended = None;
            } else if held && !cancelled && explorer_file_drag() {
                active = true;
                if tx.send(Action::FileDrag(true)).is_err() {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    });
}

fn explorer_file_drag() -> bool {
    unsafe {
        let source = GetForegroundWindow();
        let mut class = [0u16; 64];
        let len = GetClassNameW(source, &mut class) as usize;
        let class = String::from_utf16_lossy(&class[..len]);
        if !matches!(
            class.as_str(),
            "CabinetWClass" | "ExploreWClass" | "Progman" | "WorkerW"
        ) {
            return false;
        }
        let mut source_pid = 0;
        GetWindowThreadProcessId(source, Some(&mut source_pid));
        let mut data = (source_pid, false);
        let _ = EnumWindows(Some(find_drag_image), LPARAM(&mut data as *mut _ as isize));
        data.1
    }
}

unsafe extern "system" fn find_drag_image(hwnd: HWND, lp: LPARAM) -> windows::core::BOOL {
    unsafe {
        let data = &mut *(lp.0 as *mut (u32, bool));
        if IsWindowVisible(hwnd).as_bool() {
            let mut class = [0u16; 64];
            let len = GetClassNameW(hwnd, &mut class) as usize;
            if String::from_utf16_lossy(&class[..len]) == "SysDragImage" {
                let mut pid = 0;
                GetWindowThreadProcessId(hwnd, Some(&mut pid));
                if pid == data.0 {
                    data.1 = true;
                    return false.into();
                }
            }
        }
        true.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tray_starts_disconnected_and_only_reapplies_changed_or_failed_updates() {
        let mut state = TrayState::default();
        assert!(!state.connected);
        assert_eq!(state.pending(), Some(TrayUpdate::Add));
        state.applied(true);
        assert_eq!(state.pending(), None);

        state.connected = true;
        assert_eq!(state.pending(), Some(TrayUpdate::Modify));
        state.applied(false);
        assert_eq!(state.applied, Some(false));
        assert_eq!(state.pending(), Some(TrayUpdate::Modify));
        state.applied(true);
        assert_eq!(state.pending(), None);

        state.connected = false;
        assert_eq!(state.pending(), Some(TrayUpdate::Modify));
        state.applied(true);
        assert_eq!(state.pending(), None);
    }

    #[test]
    fn explorer_restore_keeps_desired_color_and_retries_a_failed_registration() {
        let mut state = TrayState {
            connected: true,
            applied: Some(true),
            dirty: false,
        };
        state.applied = None;
        assert_eq!(state.pending(), Some(TrayUpdate::Add));
        state.applied(false);
        assert!(state.connected);
        assert_eq!(state.pending(), Some(TrayUpdate::Add));
        state.applied(true);
        assert_eq!(state.applied, Some(true));
        assert_eq!(state.pending(), None);
    }

    struct IconBitmaps(ICONINFO);
    impl Drop for IconBitmaps {
        fn drop(&mut self) {
            // SAFETY: GetIconInfo creates these owned bitmap copies, independent
            // of the source HICON. Delete both even when a test assertion unwinds.
            unsafe {
                if !self.0.hbmColor.is_invalid() {
                    let _ = DeleteObject(self.0.hbmColor.into());
                }
                if !self.0.hbmMask.is_invalid() {
                    let _ = DeleteObject(self.0.hbmMask.into());
                }
            }
        }
    }

    struct MemoryDc(HDC);
    impl Drop for MemoryDc {
        fn drop(&mut self) {
            // SAFETY: the test exclusively owns this CreateCompatibleDC result.
            unsafe {
                let _ = DeleteDC(self.0);
            }
        }
    }

    fn icon_pixels(resource: u16, side: i32) -> Vec<u8> {
        let icon = Icon::load(resource, side, side).expect("embedded icon resource");
        let mut bitmaps = IconBitmaps(ICONINFO::default());
        // SAFETY: all output structs and the pixel buffer remain valid throughout
        // these calls; GetDIBits reads a bitmap not selected into any memory DC.
        unsafe {
            GetIconInfo(icon.0, &mut bitmaps.0).unwrap();
            let mut bitmap = BITMAP::default();
            assert_eq!(
                GetObjectW(
                    bitmaps.0.hbmColor.into(),
                    size_of::<BITMAP>() as i32,
                    Some(&mut bitmap as *mut _ as _),
                ),
                size_of::<BITMAP>() as i32,
            );
            assert_eq!((bitmap.bmWidth, bitmap.bmHeight), (side, side));
            let dc = MemoryDc(CreateCompatibleDC(None));
            assert!(!dc.0.is_invalid());
            let mut info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: side,
                    biHeight: -side,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut pixels = vec![0; side as usize * side as usize * 4];
            assert_eq!(
                GetDIBits(
                    dc.0,
                    bitmaps.0.hbmColor,
                    0,
                    side as u32,
                    Some(pixels.as_mut_ptr().cast()),
                    &mut info,
                    DIB_RGB_COLORS,
                ),
                side,
            );
            pixels
        }
    }

    #[test]
    fn embedded_icons_keep_requested_sizes_transparency_and_connection_colors() {
        let small = icon_pixels(1, 16);
        let large = icon_pixels(1, 32);
        assert_eq!(small.len(), 16 * 16 * 4);
        assert_eq!(large.len(), 32 * 32 * 4);

        let connected = icon_pixels(2, 32);
        let disconnected = icon_pixels(3, 32);
        let connected = connected.as_chunks::<4>().0;
        let disconnected = disconnected.as_chunks::<4>().0;
        assert!(connected.iter().any(|pixel| pixel[3] == 0));
        assert!(connected.iter().filter(|pixel| pixel[3] > 240).count() > 64);
        // GetDIBits supplies BGRA. Connected pixels are blue; offline pixels are
        // neutral gray, with identical alpha so changing state cannot move edges.
        assert!(
            connected
                .iter()
                .any(|pixel| { pixel[3] > 240 && u16::from(pixel[0]) > u16::from(pixel[2]) + 80 })
        );
        assert!(
            disconnected
                .iter()
                .filter(|pixel| pixel[3] > 0)
                .all(|pixel| {
                    pixel[0].abs_diff(pixel[1]) <= 1 && pixel[1].abs_diff(pixel[2]) <= 1
                })
        );
        assert!(
            connected
                .iter()
                .zip(disconnected)
                .all(|(blue, gray)| blue[3] == gray[3])
        );
    }
}
