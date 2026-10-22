use crate::{
    backend::{self, Event},
    model::{Model, Phase, Visibility, file_size},
    native::{Action, Shell},
    preview::{FileKind, MAX_PREVIEWS},
};
use gpui::prelude::FluentBuilder;
use gpui::*;
use omdrop_core::SendCancellation;
use std::{
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant},
};

const BLUE: u32 = 0x0a84ff;
const INK: u32 = 0xf5f5f7;
const MUTED: u32 = 0xa0a0a8;
const SURFACE: u32 = 0x27272b;
enum ExitState {
    Stopping,
    Failed(String),
}

pub struct Capsule {
    model: Model,
    orb: crate::orb::ConnectionOrb,
    visibility: Visibility,
    shell: Option<Shell>,
    focus: FocusHandle,
    tx: mpsc::Sender<Event>,
    cancel: Option<SendCancellation>,
    scan_cancel: Option<SendCancellation>,
    workers: Vec<std::thread::JoinHandle<()>>,
    firmware: crate::firmware::Model,
    firmware_cancel: Option<omdrop_flash::Cancellation>,
    firmware_worker: Option<std::thread::JoinHandle<()>>,
    picker_open: bool,
    name_input: Option<Entity<crate::name_input::NameInput>>,
    name_input_subscription: Option<Subscription>,
    settings_open: bool,
    settings_error: Option<String>,
    name_saved: bool,
    device_name: String,
    receiver: crate::receiving::ReceiverHost,
    monitor: Option<backend::Monitor>,
    exit_state: Option<ExitState>,
    receiver_status: Result<(), String>,
    receive: crate::receiving::Model,
    download_directory: Option<PathBuf>,
    receiver_declined_until: Option<Instant>,
    incoming: Option<crate::receiving::IncomingRequest>,
    drop_hovered: bool,
    opening_files: Option<u64>,
    queued_files: Vec<PathBuf>,
    queued_at: Option<Instant>,
}

impl Capsule {
    pub fn new(window: &mut Window, cx: &mut Context<Self>, files: Vec<PathBuf>) -> Self {
        let (device_name, mut settings_error) = match crate::settings::load() {
            Ok(name) => (name, None),
            Err(error) => (
                crate::settings::default_name(),
                Some(format!("读取设备名称失败：{error:#}")),
            ),
        };
        let download_directory = match crate::receiving::save_directory() {
            Ok(directory) => Some(directory),
            Err(error) => {
                settings_error = Some(format!("读取接收目录失败：{error:#}"));
                None
            }
        };
        let (tx, rx) = mpsc::channel();
        let (action_tx, actions) = mpsc::channel();
        if !files.is_empty() {
            let _ = action_tx.send(Action::Files(files));
        } else if std::env::args_os().any(|arg| arg == "--show-in-taskbar") {
            let _ = action_tx.send(Action::Show);
        }
        let shell = Shell::new(window, action_tx).expect("initialize Windows shell integration");
        let shell = Some(shell);
        if let Some(shell) = &shell {
            shell.place(false, false, false);
        }
        let receiver = crate::receiving::ReceiverHost::new(tx.clone());
        let monitor = backend::monitor(tx.clone());
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(80))
                    .await;
                if this.upgrade().is_none() {
                    break;
                }
                let events: Vec<_> = rx.try_iter().collect();
                let actions: Vec<_> = actions.try_iter().collect();
                if this
                    .update_in(cx, |this, window, cx| {
                        this.poll(events, actions, window, cx)
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        Self {
            model: Model::default(),
            orb: crate::orb::ConnectionOrb::default(),
            visibility: Visibility::Hidden,
            shell,
            focus: cx.focus_handle(),
            tx,
            cancel: None,
            scan_cancel: None,
            workers: Vec::new(),
            firmware: crate::firmware::Model::default(),
            firmware_cancel: None,
            firmware_worker: None,
            picker_open: false,
            name_input: None,
            name_input_subscription: None,
            settings_open: false,
            settings_error,
            name_saved: false,
            device_name,
            receiver,
            monitor: Some(monitor),
            exit_state: None,
            receiver_status: Err("正在启动接收服务…".into()),
            receive: crate::receiving::Model::default(),
            download_directory,
            receiver_declined_until: None,
            incoming: None,
            drop_hovered: false,
            opening_files: None,
            queued_files: Vec::new(),
            queued_at: None,
        }
    }

    fn poll(
        &mut self,
        events: Vec<Event>,
        actions: Vec<Action>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.exit_state.is_some() {
            // No late status, picker or discovery callback may restart work
            // while the receiver and radio service are being shut down.
            for event in events {
                match event {
                    Event::ShutdownDone(Ok(background_running)) => {
                        if background_running && let Some(shell) = &self.shell {
                            shell.notify(
                                "Omdrop 已退出",
                                "其他客户端或常驻服务仍在使用无线，后台将继续运行。",
                            );
                        }
                        cx.quit();
                        return;
                    }
                    Event::ShutdownDone(Err(error)) => {
                        self.exit_state = Some(ExitState::Failed(error));
                        self.expand(true, window, cx);
                        cx.notify();
                    }
                    Event::Incoming(request) => {
                        let _ = request.decision.send(false);
                    }
                    _ => {}
                }
            }
            for action in actions {
                match action {
                    Action::Quit => self.quit(window, cx),
                    Action::Show | Action::Toggle => self.expand(true, window, cx),
                    Action::Reposition => self.place(),
                    Action::RestoreTray => {
                        if let Some(shell) = self.shell.as_mut() {
                            shell.restore_tray();
                        }
                    }
                    _ => {}
                }
            }
            return;
        }
        let mut changed = false;
        self.workers.retain(|worker| !worker.is_finished());
        for event in events {
            match event {
                Event::Firmware(event) => {
                    if self.firmware.apply(event) {
                        self.firmware_worker.take();
                        self.firmware_cancel = None;
                        self.receiver = crate::receiving::ReceiverHost::new(self.tx.clone());
                        self.receiver.configure(
                            self.model
                                .connection_ready()
                                .then(|| self.device_name.clone()),
                        );
                    }
                    changed = true;
                    continue;
                }
                Event::ReceiverStatus(status) => {
                    if let Err(error) = &status {
                        self.receive
                            .interrupt_with_details("接收已中断，请重新发送".into(), error.clone());
                    }
                    self.receiver_status = status;
                    changed = true;
                    continue;
                }
                Event::DownloadDirectorySaved(result) => {
                    self.picker_open = false;
                    match result {
                        Ok(directory) => {
                            let changed_directory =
                                self.download_directory.as_ref() != Some(&directory);
                            self.download_directory = Some(directory);
                            self.settings_error = None;
                            if changed_directory {
                                self.receiver_status = Err("正在更新接收目录…".into());
                                self.receiver.reload_directory();
                            }
                        }
                        Err(error) => {
                            self.settings_error = Some(format!("保存接收目录失败：{error}"))
                        }
                    }
                    changed = true;
                    continue;
                }
                Event::Incoming(request) => {
                    if self.incoming.is_some()
                        || self.receiver_status.is_err()
                        || self.download_directory.as_ref() != Some(&request.save_dir)
                        || self.firmware.busy
                        || self.receive.active.is_some()
                        || self.model.phase.busy()
                        || self.picker_open
                        || self.name_input.is_some()
                        || !request.active()
                    {
                        let _ = request.decision.send(false);
                    } else {
                        self.expand(true, window, cx);
                        self.receive.dismiss_notice();
                        self.incoming = Some(request);
                        self.receiver_declined_until = None;
                    }
                    changed = true;
                    continue;
                }
                Event::ReceiverActivity(event) => {
                    let applied = self.receive.apply(&event);
                    if applied
                        && matches!(event, omdrop_core::ReceiverEvent::Received { .. })
                        && let (Some(shell), Some(message)) = (&self.shell, &self.receive.notice)
                    {
                        shell.notify("文件已接收", &message.message);
                    }
                    changed |= applied;
                    continue;
                }
                Event::Status(ref status) => {
                    let ready = status.as_ref().is_ok_and(backend::LinkStatus::ready);
                    // Polling continues while the sharing card is hidden; the
                    // tray must reflect the same readiness as the status label.
                    if let Some(shell) = self.shell.as_mut() {
                        shell.set_connected(ready);
                    }
                    self.receiver.configure(
                        (ready && !self.firmware.busy).then(|| self.device_name.clone()),
                    );
                    if !ready {
                        changed |= self.receive.interrupt("无线连接中断，接收未完成".into());
                    }
                }
                _ => {}
            }
            let files_ready = matches!(&event, Event::FilesValidated(id, _, Ok(_)) if *id == self.model.selection_id);
            if files_ready {
                self.clear_previews(window);
            }
            changed |= self.model.apply(event);
            let reconnected = std::mem::take(&mut self.model.rescan_requested);
            if files_ready || reconnected {
                self.scan(cx);
            }
        }
        for action in actions {
            match action {
                Action::Toggle => self.expand(self.visibility != Visibility::Share, window, cx),
                Action::Show => self.expand(true, window, cx),
                Action::Close => self.expand(false, window, cx),
                Action::Files(paths) => {
                    self.queued_files.extend(paths);
                    self.queued_at = Some(Instant::now());
                    self.expand(true, window, cx);
                }
                Action::FilesDropped(paths) => self.accept_files(paths, window, cx),
                Action::DropHover(active) => {
                    self.drop_hovered = active;
                    changed = true;
                }
                Action::FilesPicked(result) => {
                    self.picker_open = false;
                    match result {
                        Ok(Some(paths)) => self.accept_files(paths, window, cx),
                        Err(error) => {
                            self.model.error = Some(format!("打开文件选择器失败：{error}"))
                        }
                        Ok(None) => {}
                    }
                    changed = true;
                }
                Action::FilesOpened(id, result) => {
                    if self.opening_files == Some(id) {
                        self.opening_files = None;
                        if id == self.model.selection_id
                            && self.model.phase == Phase::Ready
                            && let Err(error) = result
                        {
                            self.model.error = Some(error);
                        }
                        changed = true;
                    }
                }
                Action::DownloadDirectoryChosen(result) => {
                    match result {
                        Ok(Some(directory)) => {
                            let events = self.tx.clone();
                            self.workers.push(std::thread::spawn(move || {
                                let result = omdrop_host::save_download_dir(&directory)
                                    .map_err(|error| format!("{error:#}"));
                                let _ = events.send(Event::DownloadDirectorySaved(result));
                            }));
                        }
                        Ok(None) => self.picker_open = false,
                        Err(error) => {
                            self.picker_open = false;
                            self.settings_error = Some(error);
                        }
                    }
                    changed = true;
                }
                Action::FileDrag(active) => {
                    if !self.model.phase.busy() && !self.picker_open && self.name_input.is_none()
                        || !active
                    {
                        self.visibility.file_drag(active);
                        self.place();
                        changed = true;
                    }
                }
                Action::ShellError(error) => {
                    self.model.error = Some(error);
                    changed = true;
                }
                Action::Choose => self.choose(window, cx),
                Action::Quit => {
                    self.quit(window, cx);
                    return;
                }
                Action::Reposition => {
                    self.place();
                }
                Action::RestoreTray => {
                    if let Some(shell) = self.shell.as_mut() {
                        shell.restore_tray();
                    }
                }
            }
        }
        if self
            .incoming
            .as_ref()
            .is_some_and(|request| !request.active())
        {
            self.answer_incoming(false, cx);
            self.receive.show_notice(
                "接收请求已结束，请重新发送",
                crate::receiving::NoticeKind::Info,
            );
            changed = true;
        }
        changed |= self.receive.expire_notice(Instant::now());
        if self
            .receiver_declined_until
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.receiver_declined_until = None;
            changed = true;
        }
        // Static Explorer verbs can launch once per selected file; coalesce the
        // batch before doing filesystem metadata work or device discovery.
        if self
            .queued_at
            .is_some_and(|at| at.elapsed() >= Duration::from_millis(350))
        {
            self.queued_at = None;
            let paths = std::mem::take(&mut self.queued_files);
            self.accept_files(paths, window, cx);
        }
        if changed {
            cx.notify();
        }
    }

    fn quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.begin_quit(false, window, cx);
    }

    fn begin_quit(
        &mut self,
        firmware_confirmed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.exit_state, Some(ExitState::Stopping)) {
            return;
        }
        if !self.firmware.request_exit(firmware_confirmed) {
            self.expand(true, window, cx);
            self.firmware.open = true;
            cx.notify();
            return;
        }
        if let Some(cancel) = &self.firmware_cancel {
            cancel.cancel();
        }
        if let Some(cancel) = &self.scan_cancel {
            cancel.cancel();
        }
        if let Some(cancel) = &self.cancel {
            cancel.cancel();
        }
        if let Some(request) = self.incoming.take() {
            let _ = request.decision.send(false);
        }
        self.exit_state = Some(ExitState::Stopping);
        if let Some(shell) = self.shell.as_mut() {
            shell.set_connected(false);
        }
        self.finish_name_edit(window);
        self.settings_open = false;
        self.expand(true, window, cx);
        let mut workers = std::mem::take(&mut self.workers);
        if let Some(worker) = self.firmware_worker.take() {
            workers.push(worker);
        }
        backend::shutdown(
            self.monitor.take(),
            self.receiver.shutdown(),
            workers,
            self.tx.clone(),
        );
        cx.notify();
    }
    fn place(&self) {
        if let Some(shell) = &self.shell {
            shell.place(
                self.visibility == Visibility::Share,
                self.visibility != Visibility::Hidden,
                false,
            );
        }
    }
    fn expand(&mut self, expanded: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !expanded {
            self.receive.reset_notice_hover();
            self.answer_incoming(false, cx);
        }
        self.drop_hovered = false;
        self.settings_open = false;
        self.finish_name_edit(window);
        self.name_saved = false;
        self.visibility = if expanded {
            Visibility::Share
        } else {
            Visibility::Hidden
        };
        if let Some(shell) = &self.shell {
            shell.place(expanded, expanded, expanded);
        }
        if expanded {
            self.focus.focus(window);
        }
        cx.notify();
    }
    fn can_open_files(&self) -> bool {
        self.model.phase == Phase::Ready
            && !self.model.files.is_empty()
            && !self.model.validating
            && !self.picker_open
            && self.name_input.is_none()
            && self.queued_at.is_none()
            && self.opening_files.is_none()
    }
    fn open_files(&mut self, cx: &mut Context<Self>) {
        if !self.can_open_files() {
            return;
        }
        if let Some(shell) = &self.shell {
            match shell.open_files(self.model.selection_id, self.model.files.clone()) {
                Ok(()) => {
                    self.opening_files = Some(self.model.selection_id);
                    self.model.error = None;
                }
                Err(error) => self.model.error = Some(format!("打开文件失败：{error}")),
            }
            cx.notify();
        }
    }
    fn choose(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.firmware.busy
            || self.model.phase.busy()
            || self.picker_open
            || self.name_input.is_some()
            || self.incoming.is_some()
        {
            return;
        }
        self.expand(true, window, cx);
        self.picker_open = true;
        if let Some(shell) = &self.shell
            && let Err(error) = shell.choose_files()
        {
            self.picker_open = false;
            self.model.error = Some(format!("打开文件选择器失败：{error}"));
        }
        cx.notify();
    }
    fn edit_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.firmware.busy
            || self.model.phase.busy()
            || self.picker_open
            || self.name_input.is_some()
            || self.incoming.is_some()
            || self.receive.active.is_some()
        {
            return;
        }
        let input = cx.new(|cx| crate::name_input::NameInput::new(self.device_name.clone(), cx));
        self.name_input_subscription =
            Some(
                cx.subscribe_in(&input, window, |this, _, event, window, cx| match event {
                    crate::name_input::NameInputEvent::Changed => {
                        this.settings_error = None;
                        this.name_saved = false;
                        cx.notify();
                    }
                    crate::name_input::NameInputEvent::Submit => this.save_name(window, cx),
                    crate::name_input::NameInputEvent::Cancel => this.cancel_name(window, cx),
                }),
            );
        input.focus_handle(cx).focus(window);
        self.name_input = Some(input);
        self.settings_error = None;
        self.name_saved = false;
        cx.notify();
    }
    fn can_change_download_directory(&self) -> bool {
        !self.firmware.busy
            && !self.picker_open
            && self.name_input.is_none()
            && self.incoming.is_none()
            && self.receive.active.is_none()
            && !self.receiver.has_active_transfers()
    }

    fn choose_download_directory(&mut self, cx: &mut Context<Self>) {
        if !self.can_change_download_directory() {
            return;
        }
        if let Some(shell) = &self.shell {
            match shell.choose_download_directory() {
                Ok(()) => {
                    self.picker_open = true;
                    self.settings_error = None;
                }
                Err(error) => self.settings_error = Some(format!("{error:#}")),
            }
        }
        cx.notify();
    }
    fn finish_name_edit(&mut self, window: &mut Window) {
        self.name_input_subscription = None;
        self.name_input = None;
        self.focus.focus(window);
    }
    fn cancel_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.finish_name_edit(window);
        self.settings_error = None;
        self.name_saved = false;
        cx.notify();
    }
    fn save_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(input) = self.name_input.as_ref() else {
            return;
        };
        if input.read(cx).is_composing() || self.receive.active.is_some() {
            return;
        }
        let draft = input.read(cx).text().to_owned();
        let result =
            crate::settings::path().and_then(|path| crate::settings::save_to(&path, &draft));
        match result {
            Ok(name) => {
                if self.device_name != name {
                    self.receiver_status = Err(
                        "\u{6b63}\u{5728}\u{66f4}\u{65b0}\u{8bbe}\u{5907}\u{5e7f}\u{64ad}\u{2026}"
                            .into(),
                    );
                }
                self.device_name = name;
                self.receiver.configure(
                    self.model
                        .connection_ready()
                        .then(|| self.device_name.clone()),
                );
                self.finish_name_edit(window);
                self.settings_error = None;
                self.name_saved = true;
            }
            Err(error) => self.settings_error = Some(format!("{error:#}")),
        }
        cx.notify();
    }
    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.firmware.busy
            || self.model.phase.busy()
            || self.picker_open
            || self.name_input.is_some()
            || self.incoming.is_some()
        {
            return;
        }
        self.settings_open = true;
        self.firmware.open = false;
        self.name_saved = false;
        self.focus.focus(window);
        cx.notify();
    }
    fn back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.firmware.open {
            self.firmware.open = false;
            self.firmware.confirm_exit = false;
            cx.notify();
        } else if self.settings_open {
            self.settings_open = false;
            self.finish_name_edit(window);
            self.name_saved = false;
            self.focus.focus(window);
            cx.notify();
        } else {
            self.reset(window, cx);
        }
    }
    fn accept_files(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        if paths.is_empty() {
            return;
        }
        self.expand(true, window, cx);
        if self.firmware.busy || self.model.phase.busy() {
            self.model.error = Some("正在操作设备，请完成或取消后再添加文件。".into());
            cx.notify();
            return;
        }
        // Every new picker/drop/Explorer batch replaces the previous selection.
        // Retaining an older batch here could send files the user did not select.
        let mut selection = Vec::new();
        for path in paths {
            if !selection.contains(&path) {
                selection.push(path);
            }
        }
        if selection.is_empty() {
            return;
        }
        self.receive.dismiss_notice();
        self.model.selection_id += 1;
        self.model.validating = true;
        self.model.error = None;
        backend::validate(self.model.selection_id, selection, self.tx.clone());
        cx.notify();
    }
    fn scan(&mut self, cx: &mut Context<Self>) {
        if self.firmware.busy || self.model.scanning || self.model.phase.busy() {
            return;
        }
        self.model.scan_id += 1;
        self.model.scanning = true;
        self.model.peers.clear();
        self.model.selected = None;
        self.model.error = None;
        if let Some(cancel) = self.scan_cancel.take() {
            cancel.cancel();
        }
        let cancel = SendCancellation::default();
        self.scan_cancel = Some(cancel.clone());
        self.workers.push(backend::scan(
            self.model.scan_id,
            self.device_name.clone(),
            cancel,
            self.tx.clone(),
        ));
        cx.notify();
    }
    fn send(&mut self, cx: &mut Context<Self>) {
        if self.firmware.busy
            || self.picker_open
            || self.name_input.is_some()
            || self.queued_at.is_some()
        {
            return;
        }
        let Some(peer) = self.model.begin_send() else {
            return;
        };
        let cancel = SendCancellation::default();
        self.cancel = Some(cancel.clone());
        if let Some(cancel) = self.scan_cancel.take() {
            cancel.cancel();
        }
        self.workers.push(backend::send(
            self.model.send_id,
            peer,
            self.model.files.clone(),
            self.device_name.clone(),
            cancel,
            self.tx.clone(),
        ));
        cx.notify();
    }
    fn cancel(&mut self, cx: &mut Context<Self>) {
        if let Some(cancel) = &self.cancel {
            cancel.cancel();
        }
        self.model.phase = Phase::Cancelling;
        cx.notify();
    }
    fn clear_previews(&mut self, window: &mut Window) {
        for image in self.model.previews.drain(..).flatten() {
            let _ = window.drop_image(image);
        }
    }
    fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.firmware.busy || self.model.phase.busy() || self.picker_open {
            return;
        }
        self.clear_previews(window);
        self.model.reset_selection();
        self.queued_files.clear();
        self.queued_at = None;
        self.cancel = None;
        if let Some(cancel) = self.scan_cancel.take() {
            cancel.cancel();
        }
        cx.notify();
    }
    fn status_text(&self) -> (&'static str, u32) {
        match &self.model.status {
            Some(Ok(status)) if status.ready() => ("已连接", 0x30d158),
            Some(Ok(_)) => ("等待无线桥接", 0xff9f0a),
            Some(Err(_)) => ("后台服务未连接", 0xff9f0a),
            None => ("正在连接", MUTED),
        }
    }

    fn pill(&self) -> AnyElement {
        let subtitle = if self.model.phase.busy() {
            self.model.phase.title()
        } else if self.model.phase == Phase::Complete {
            "文件已送达"
        } else {
            "拖入文件即可发送"
        };
        div()
            .id("capsule")
            .w_full()
            .h(px(56.))
            .rounded_full()
            .bg(rgb(0x1c1c1f))
            .border_1()
            .border_color(rgba(0xffffff24))
            .flex()
            .items_center()
            .px(px(16.))
            .gap(px(12.))
            .when(self.drop_hovered, |pill| {
                pill.bg(rgb(0x163353)).border_color(rgb(BLUE))
            })
            .child(crate::brand::mark(30.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .text_size(px(14.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Omdrop"),
                    )
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(rgb(MUTED))
                            .child(subtitle),
                    ),
            )
            .into_any_element()
    }

    fn exit_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let error = match &self.exit_state {
            Some(ExitState::Failed(error)) => Some(error),
            _ => None,
        };
        div()
            .size_full()
            .rounded(px(crate::window_frame::CORNER_RADIUS))
            .bg(rgb(0x1c1c1f))
            .border_1()
            .border_color(rgba(0xffffff24))
            .flex()
            .flex_col()
            .p(px(22.))
            .gap(px(18.))
            .child(crate::brand::mark(30.))
            .child(
                div()
                    .text_size(px(17.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(if error.is_some() {
                        "后台服务未能停止"
                    } else {
                        "正在退出 Omdrop…"
                    }),
            )
            .child(div().text_size(px(13.)).text_color(rgb(MUTED)).child(
                error.cloned().unwrap_or_else(|| {
                    "正在结束本机任务并释放设备。其他客户端仍在使用时，后台会继续运行。".into()
                }),
            ))
            .when(error.is_some(), |panel| {
                panel.child(
                    button("retry-quit", "重试退出", true)
                        .on_click(cx.listener(|this, _, window, cx| this.quit(window, cx))),
                )
            })
            .into_any_element()
    }

    fn panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let (status, color) = self.status_text();
        let show_back = self.incoming.is_none()
            && (self.settings_open
                || self.firmware.open
                || (self.model.phase == Phase::Ready
                    && (!self.model.files.is_empty() || self.model.validating)));
        div()
            .id("panel")
            .size_full()
            .rounded(px(crate::window_frame::CORNER_RADIUS))
            .bg(rgb(0x1c1c1f))
            .border_1()
            .border_color(rgba(0xffffff24))
            .flex()
            .flex_col()
            .p(px(22.))
            .overflow_hidden()
            .when(self.drop_hovered, |panel| {
                panel.border_color(rgb(BLUE)).bg(rgb(0x202b38))
            })
            .child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .justify_between()
                    .mb(px(if show_back { 4. } else { 18. }))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(9.))
                            .child(crate::brand::mark(25.))
                            .child(
                                div()
                                    .text_size(px(17.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Omdrop"),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .when(
                                !self.settings_open
                                    && !self.firmware.open
                                    && self.incoming.is_none(),
                                |controls| {
                                    controls.child(
                                        div()
                                            .id("open-firmware")
                                            .h(px(28.))
                                            .px(px(8.))
                                            .flex_shrink_0()
                                            .rounded_full()
                                            .bg(rgb(SURFACE))
                                            .flex()
                                            .items_center()
                                            .text_size(px(11.))
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(rgb(MUTED))
                                            .child("设备固件")
                                            .when(self.can_open_firmware(), |control| {
                                                control.cursor_pointer().hover(|s| {
                                                    s.bg(rgb(0x393940)).text_color(rgb(INK))
                                                })
                                            })
                                            .when(!self.can_open_firmware(), |control| {
                                                control.opacity(0.4)
                                            })
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.open_firmware(window, cx)
                                            })),
                                    )
                                },
                            )
                            .when(!self.settings_open, |controls| {
                                controls.child(
                                    div()
                                        .id("settings")
                                        .size(px(28.))
                                        .rounded_full()
                                        .bg(rgb(SURFACE))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .text_color(rgb(MUTED))
                                        .child(settings_gear())
                                        .when(
                                            !self.model.phase.busy()
                                                && !self.picker_open
                                                && self.name_input.is_none(),
                                            |control| {
                                                control.cursor_pointer().hover(|s| {
                                                    s.bg(rgb(0x393940)).text_color(rgb(INK))
                                                })
                                            },
                                        )
                                        .when(
                                            self.model.phase.busy()
                                                || self.picker_open
                                                || self.name_input.is_some(),
                                            |control| control.opacity(0.4),
                                        )
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.open_settings(window, cx)
                                        })),
                                )
                            })
                            .child(
                                div()
                                    .id("close")
                                    .cursor_pointer()
                                    .size(px(28.))
                                    .rounded_full()
                                    .bg(rgb(SURFACE))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_color(rgb(MUTED))
                                    .text_size(px(20.))
                                    .child("×")
                                    .hover(|s| s.bg(rgb(0x393940)))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.expand(false, window, cx)
                                    })),
                            ),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .when(show_back, |content| {
                        content.child(
                            div()
                                .relative()
                                .flex()
                                .flex_shrink_0()
                                .w_full()
                                .h(px(24.))
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .id("back")
                                        .h(px(24.))
                                        .px(px(8.))
                                        .flex()
                                        .items_center()
                                        .gap(px(4.))
                                        .rounded(px(8.))
                                        .cursor_pointer()
                                        .text_color(rgb(MUTED))
                                        .child(back_chevron())
                                        .child(div().text_size(px(11.)).line_height(px(14.)).child(
                                            if self.settings_open || self.firmware.open {
                                                "返回分享"
                                            } else {
                                                "返回"
                                            },
                                        ))
                                        .hover(|s| s.bg(rgb(SURFACE)).text_color(rgb(INK)))
                                        .on_click(
                                            cx.listener(|this, _, window, cx| {
                                                this.back(window, cx)
                                            }),
                                        ),
                                )
                                .when(self.settings_open || self.firmware.open, |row| {
                                    row.child(
                                        div()
                                            .id("page-title")
                                            .absolute()
                                            .left(px(80.))
                                            .right(px(80.))
                                            .top(px(0.))
                                            .h(px(24.))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .text_size(px(13.))
                                            .line_height(px(18.))
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(if self.firmware.open {
                                                "设备固件"
                                            } else {
                                                "设置"
                                            }),
                                    )
                                }),
                        )
                    })
                    .child(if self.incoming.is_some() {
                        self.incoming_view(cx)
                    } else if self.firmware.open {
                        self.firmware_view(cx)
                    } else if self.settings_open {
                        self.settings_view(cx)
                    } else if self.model.phase != Phase::Ready {
                        self.transfer_view(cx)
                    } else if self.receive.active.is_some() && self.model.files.is_empty() {
                        self.receiving_view(false)
                    } else if self.model.files.is_empty() {
                        self.empty_view(cx)
                    } else {
                        self.files_view(cx)
                    }),
            )
            .when(self.incoming.is_none(), |panel| {
                panel
                    .when(
                        self.receive.active.is_some()
                            && (self.model.phase != Phase::Ready
                                || !self.model.files.is_empty()
                                || self.settings_open
                                || self.firmware.open),
                        |panel| panel.child(self.receiving_view(true)),
                    )
                    .children(self.receive.notice.as_ref().map(|notice| {
                        let id = notice.id;
                        let color = match notice.kind {
                            crate::receiving::NoticeKind::Info => MUTED,
                            crate::receiving::NoticeKind::Success => 0x30d158,
                            crate::receiving::NoticeKind::Error => 0xffb56b,
                        };
                        crate::share_layout::notice_banner(
                            crate::share_layout::NoticeContent {
                                id,
                                message: notice.message.clone(),
                                full_text: notice.full_text.clone(),
                                color,
                                expanded: notice.details_visible,
                                copied: notice.copied,
                            },
                            cx.listener(move |this, hovered, _, cx| {
                                if this.receive.set_notice_hovered(
                                    id,
                                    crate::receiving::NoticeArea::Banner,
                                    *hovered,
                                ) {
                                    cx.notify();
                                }
                            }),
                            cx.listener(move |this, hovered, _, cx| {
                                if this.receive.set_notice_hovered(
                                    id,
                                    crate::receiving::NoticeArea::Details,
                                    *hovered,
                                ) {
                                    cx.notify();
                                }
                            }),
                            cx.listener(move |this, _, _, cx| {
                                if let Some(text) = this.receive.copy_notice(id) {
                                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                                    cx.notify();
                                }
                            }),
                            cx.listener(move |this, _, _, cx| {
                                if this
                                    .receive
                                    .notice
                                    .as_ref()
                                    .is_some_and(|notice| notice.id == id)
                                    && this.receive.dismiss_notice()
                                {
                                    cx.notify();
                                }
                            }),
                        )
                    }))
                    .when(self.receiver_declined_until.is_some(), |panel| {
                        panel.child(
                            div()
                                .id("receiver-declined-notice")
                                .w_full()
                                .text_center()
                                .text_size(px(10.))
                                .text_color(rgb(MUTED))
                                .child("已拒绝接收"),
                        )
                    })
            })
            .child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .mt(px(18.))
                    .justify_between()
                    .items_center()
                    .text_size(px(10.))
                    .text_color(rgb(MUTED))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(div().size(px(5.)).rounded_full().bg(rgb(color)))
                            .child(status),
                    )
                    .child(if self.name_input.is_some() {
                        "Enter 保存 · Esc 取消"
                    } else if self.settings_open || self.firmware.open {
                        "Esc  返回分享"
                    } else if self.picker_open {
                        "正在选择文件…"
                    } else if self.model.validating {
                        "正在读取文件信息…"
                    } else {
                        "Ctrl+O  选择文件"
                    }),
            )
            .into_any_element()
    }

    fn can_open_firmware(&self) -> bool {
        !self.model.phase.busy()
            && !self.picker_open
            && self.name_input.is_none()
            && self.incoming.is_none()
    }

    fn open_firmware(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_open_firmware() {
            return;
        }
        self.finish_name_edit(window);
        self.settings_open = false;
        self.firmware.open = true;
        if !self.firmware.busy && !self.firmware.loading {
            self.firmware.loading = true;
            crate::firmware::refresh(self.tx.clone());
        }
        cx.notify();
    }
    fn firmware_transfer_active(&self) -> bool {
        self.model.phase.busy()
            || self.receive.active.is_some()
            || self.incoming.is_some()
            || self.receiver.has_active_transfers()
    }
    fn start_firmware(&mut self, cx: &mut Context<Self>) {
        if !self.firmware.can_start(self.firmware_transfer_active()) {
            return;
        }
        let paths = omdrop_flash::installed_manifest()
            .and_then(|manifest| Ok((manifest, omdrop_flash::installed_tool()?)));
        let (manifest_path, tool_path) = match paths {
            Ok(paths) => paths,
            Err(error) => {
                self.firmware.error = Some(error.to_string());
                cx.notify();
                return;
            }
        };
        if let Some(cancel) = self.scan_cancel.take() {
            cancel.cancel();
        }
        self.model.scan_id += 1;
        self.model.scanning = false;
        self.firmware.busy = true;
        self.firmware.ready = false;
        self.firmware.confirm_exit = false;
        self.firmware.stage = None;
        self.firmware.progress = None;
        self.firmware.cancelling = false;
        self.firmware.error = None;
        self.firmware.message = "正在停止空闲接收并申请设备维护…".into();
        let cancel = omdrop_flash::Cancellation::default();
        self.firmware_cancel = Some(cancel.clone());
        let options = omdrop_flash::FlashOptions {
            manifest_path,
            tool_path,
            port: self.firmware.selected.clone().unwrap(),
            board: self.firmware.package.as_ref().unwrap().board.clone(),
            mode: self.firmware.mode,
        };
        self.firmware_worker = Some(crate::firmware::start(
            options,
            cancel,
            self.receiver.shutdown(),
            std::mem::take(&mut self.workers),
            self.tx.clone(),
        ));
        cx.notify();
    }
    fn firmware_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let transfer_active = self.firmware_transfer_active();
        let can_start = self.firmware.can_start(transfer_active);
        let mut body = div()
            .w_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(
                firmware_card()
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(FontWeight::MEDIUM)
                            .child(self.firmware.package.as_ref().map_or_else(
                                || "固件安装".to_string(),
                                |package| package.board.clone(),
                            )),
                    )
                    .child(firmware_hint(
                        self.firmware.package.as_ref().map_or_else(
                            || "未找到可用的固件包".to_string(),
                            |package| {
                                format!(
                                    "{} · {} MiB",
                                    package.chip,
                                    package.flash_size / (1024 * 1024)
                                )
                            },
                        ),
                        MUTED,
                    ))
                    .child(firmware_hint(
                        format!(
                            "固件包：{} · 设备版本：未知",
                            self.firmware.version.as_deref().unwrap_or("未找到")
                        ),
                        MUTED,
                    )),
            );
        if self.firmware.confirm_exit && self.firmware.busy {
            body = body.child(
                firmware_card()
                    .child(
                        div()
                            .text_size(px(12.))
                            .font_weight(FontWeight::MEDIUM)
                            .child("停止安装并退出？"),
                    )
                    .child(firmware_hint(
                        "正在写入固件。取消后可能需要完整重刷；退出会等待烧录进程停止并释放串口。",
                        0xffb56b,
                    )),
            );
            let actions = div()
                .flex()
                .gap(px(8.))
                .child(div().flex_1().min_w_0().child(
                    firmware_button("keep-flashing", "继续安装", true).on_click(cx.listener(
                        |this, _, _, cx| {
                            this.firmware.confirm_exit = false;
                            cx.notify();
                        },
                    )),
                ))
                .child(div().flex_1().min_w_0().child(
                    firmware_button("cancel-flash-quit", "取消并退出", false).on_click(
                        cx.listener(|this, _, window, cx| this.begin_quit(true, window, cx)),
                    ),
                ));
            return crate::share_layout::page_with_actions(
                "firmware-page",
                body.into_any_element(),
                actions.into_any_element(),
            )
            .into_any_element();
        }
        if self.firmware.busy {
            let mut status = firmware_card()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_size(px(12.))
                                .font_weight(FontWeight::MEDIUM)
                                .child(self.firmware.stage_label()),
                        )
                        .children(self.firmware.progress.map(|progress| {
                            div()
                                .text_size(px(11.))
                                .text_color(rgb(MUTED))
                                .child(format!("{}%", progress.min(100)))
                        })),
                )
                .child(firmware_hint(self.firmware.message.clone(), MUTED));
            if let Some(progress) = self.firmware.progress {
                status = status.child(
                    div()
                        .h(px(5.))
                        .flex_shrink_0()
                        .w_full()
                        .rounded_full()
                        .bg(rgb(0x3a3a40))
                        .child(
                            div()
                                .h_full()
                                .w(relative(progress.min(100) as f32 / 100.))
                                .rounded_full()
                                .bg(rgb(BLUE)),
                        ),
                );
            }
            body = body.child(status).child(firmware_hint(
                "写入期间请勿拔线。取消会停止烧录进程，可能需要完整重刷。",
                0xffb56b,
            ));
            let action = firmware_button(
                "cancel-firmware",
                if self.firmware.cancelling {
                    "正在停止…"
                } else {
                    "取消固件操作"
                },
                false,
            )
            .when(self.firmware.cancelling, |button| {
                button.opacity(0.4).cursor(CursorStyle::Arrow)
            })
            .on_click(cx.listener(|this, _, _, cx| {
                if this.firmware.busy && !this.firmware.cancelling {
                    if let Some(cancel) = &this.firmware_cancel {
                        cancel.cancel();
                    }
                    this.firmware.cancelling = true;
                    cx.notify();
                }
            }));
            return crate::share_layout::page_with_actions(
                "firmware-page",
                body.into_any_element(),
                action.into_any_element(),
            )
            .into_any_element();
        }
        if self.firmware.ready || self.firmware.error.is_some() {
            let status = match &self.firmware.error {
                Some(error) => firmware_hint(error.clone(), 0xffb56b),
                None => firmware_hint(self.firmware.message.clone(), 0x30d158),
            };
            body = body.child(firmware_card().child(status));
        }
        let mut devices = firmware_card().child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_size(px(12.))
                        .font_weight(FontWeight::MEDIUM)
                        .child("连接设备"),
                )
                .child(
                    div()
                        .id("refresh-firmware")
                        .h(px(24.))
                        .px(px(6.))
                        .rounded(px(6.))
                        .flex()
                        .items_center()
                        .text_size(px(11.))
                        .text_color(rgb(BLUE))
                        .child(if self.firmware.loading {
                            "检查中…"
                        } else {
                            "刷新"
                        })
                        .when(!self.firmware.loading, |button| {
                            button.cursor_pointer().hover(|s| s.bg(rgb(0x393940)))
                        })
                        .when(self.firmware.loading, |button| button.opacity(0.4))
                        .on_click(cx.listener(|this, _, _, cx| {
                            if !this.firmware.loading && !this.firmware.busy {
                                this.firmware.loading = true;
                                crate::firmware::refresh(this.tx.clone());
                                cx.notify();
                            }
                        })),
                ),
        );
        if self.firmware.devices.is_empty() {
            devices = devices.child(firmware_hint(
                if self.firmware.loading {
                    "正在查找设备和检查固件包…"
                } else {
                    "未发现串口。请使用数据线连接设备后刷新；必要时按住 G0 再点按 RST 进入下载模式。"
                }, MUTED,
            ));
        }
        for (index, device) in self.firmware.devices.iter().enumerate() {
            let port = device.port.clone();
            let selected = self.firmware.selected.as_ref() == Some(&port);
            devices = devices.child(
                div()
                    .id(("firmware-port", index))
                    .w_full()
                    .min_w_0()
                    .p(px(8.))
                    .flex_shrink_0()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(rgb(if selected { BLUE } else { 0x3a3a40 }))
                    .bg(rgb(if selected { 0x163353 } else { 0x202024 }))
                    .flex()
                    .items_start()
                    .gap(px(8.))
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(if selected { BLUE } else { MUTED }))
                            .child(if selected { "●" } else { "○" }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(port.clone()),
                            )
                            .child(firmware_hint(device.label.clone(), MUTED)),
                    )
                    .when(!self.firmware.loading, |row| row.cursor_pointer())
                    .when(self.firmware.loading, |row| row.opacity(0.5))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.firmware.loading || this.firmware.busy {
                            return;
                        }
                        if this.firmware.selected.as_ref() != Some(&port) {
                            this.firmware.board_confirmed = false;
                            this.firmware.ready = false;
                        }
                        this.firmware.selected = Some(port.clone());
                        cx.notify();
                    })),
            );
        }
        body = body.child(devices);
        let upgrade = self.firmware.mode == omdrop_flash::InstallMode::Upgrade;
        let mut mode_buttons = div().flex().gap(px(6.));
        for (id, label, mode) in [
            (
                "firmware-upgrade",
                "保留设置更新",
                omdrop_flash::InstallMode::Upgrade,
            ),
            (
                "firmware-reinstall",
                "首次 / 重新安装",
                omdrop_flash::InstallMode::Reinstall,
            ),
        ] {
            let selected = self.firmware.mode == mode;
            mode_buttons = mode_buttons.child(
                div()
                    .id(id)
                    .flex_1()
                    .min_w_0()
                    .h(px(32.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(rgb(if selected { BLUE } else { 0x3a3a40 }))
                    .bg(rgb(if selected { 0x163353 } else { 0x202024 }))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(11.))
                    .text_color(rgb(if selected { INK } else { MUTED }))
                    .cursor_pointer()
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.firmware.busy {
                            this.firmware.mode = mode;
                            cx.notify();
                        }
                    })),
            );
        }
        body = body.child(
            firmware_card()
                .child(
                    div()
                        .text_size(px(12.))
                        .font_weight(FontWeight::MEDIUM)
                        .child("安装选项"),
                )
                .child(mode_buttons)
                .child(firmware_hint(
                    if upgrade {
                        "保留现有设置；需识别到 Omdrop 固件，且分区布局一致。"
                    } else {
                        "将替换原程序和分区布局，其他固件数据可能丢失。"
                    },
                    if upgrade { MUTED } else { 0xffb56b },
                ))
                .child(
                    div()
                        .id("confirm-board")
                        .py(px(4.))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .cursor_pointer()
                        .child(
                            div()
                                .size(px(16.))
                                .flex_shrink_0()
                                .rounded(px(3.))
                                .border_1()
                                .border_color(rgb(if self.firmware.board_confirmed {
                                    BLUE
                                } else {
                                    MUTED
                                }))
                                .bg(rgb(if self.firmware.board_confirmed {
                                    BLUE
                                } else {
                                    0x202024
                                }))
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(px(11.))
                                .text_color(rgb(0xffffff))
                                .child(if self.firmware.board_confirmed {
                                    "✓"
                                } else {
                                    ""
                                }),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(11.))
                                .whitespace_normal()
                                .child("我已确认设备板型、芯片和 Flash 容量与上方固件包匹配"),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            if !this.firmware.busy {
                                this.firmware.board_confirmed = !this.firmware.board_confirmed;
                                cx.notify();
                            }
                        })),
                )
                .child(firmware_hint("芯片型号不能替代板型确认。", MUTED)),
        );
        if transfer_active {
            body = body.child(firmware_hint(
                "正在传输文件，请先完成或取消传输。其他客户端占用设备时也无法安装。",
                0xffb56b,
            ));
        }
        let action = firmware_button(
            "install-firmware",
            if self.firmware.error.is_some() {
                "重试安装"
            } else if upgrade {
                "开始更新"
            } else {
                "开始安装"
            },
            true,
        )
        .when(!can_start, |button| {
            button.opacity(0.4).cursor(CursorStyle::Arrow)
        })
        .on_click(cx.listener(|this, _, _, cx| this.start_firmware(cx)));
        crate::share_layout::page_with_actions(
            "firmware-page",
            body.into_any_element(),
            action.into_any_element(),
        )
        .into_any_element()
    }

    fn settings_view(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("settings-page")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(
                div()
                    .flex_shrink_0()
                    .rounded(px(16.))
                    .bg(rgb(SURFACE))
                    .border_1()
                    .border_color(rgba(0xffffff12))
                    .p(px(16.))
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(rgb(MUTED))
                                    .child("设备名称"),
                            )
                            .when(self.name_input.is_none(), |row| {
                                row.child(
                                    div()
                                        .id("edit-device-name")
                                        .h(px(24.))
                                        .px(px(6.))
                                        .rounded(px(6.))
                                        .flex()
                                        .items_center()
                                        .text_size(px(11.))
                                        .text_color(rgb(BLUE))
                                        .child("修改")
                                        .cursor_pointer()
                                        .hover(|s| s.bg(rgb(0x32323a)))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.edit_name(window, cx)
                                        })),
                                )
                            }),
                    )
                    .child(if let Some(input) = &self.name_input {
                        input.clone().into_any_element()
                    } else {
                        div()
                            .id("settings-device-name")
                            .w_full()
                            .max_h(px(88.))
                            .overflow_y_scroll()
                            .whitespace_normal()
                            .text_size(px(17.))
                            .line_height(px(22.))
                            .font_weight(FontWeight::MEDIUM)
                            .child(self.device_name.clone())
                            .into_any_element()
                    })
                    .child(
                        div()
                            .text_size(px(11.))
                            .line_height(px(17.))
                            .text_color(rgb(MUTED))
                            .child("附近设备会在隔空投送列表和发送请求中看到这个名称。"),
                    )
                    .when_some(self.name_input.as_ref(), |card, input| {
                        let length = input.read(cx).text().chars().count();
                        card.child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .text_size(px(10.))
                                        .text_color(rgb(
                                            if length > crate::settings::MAX_NAME_CHARS {
                                                0xffb56b
                                            } else {
                                                MUTED
                                            },
                                        ))
                                        .child(format!(
                                            "{length} / {} 字符",
                                            crate::settings::MAX_NAME_CHARS
                                        )),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .gap(px(8.))
                                        .child(
                                            div()
                                                .id("cancel-device-name")
                                                .h(px(30.))
                                                .px(px(12.))
                                                .rounded(px(8.))
                                                .bg(rgb(0x393940))
                                                .flex()
                                                .items_center()
                                                .text_size(px(11.))
                                                .cursor_pointer()
                                                .child("取消")
                                                .hover(|s| s.bg(rgb(0x484850)))
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.cancel_name(window, cx)
                                                })),
                                        )
                                        .child(
                                            div()
                                                .id("save-device-name")
                                                .h(px(30.))
                                                .px(px(16.))
                                                .rounded(px(8.))
                                                .bg(rgb(BLUE))
                                                .flex()
                                                .items_center()
                                                .text_size(px(11.))
                                                .font_weight(FontWeight::MEDIUM)
                                                .cursor_pointer()
                                                .child("保存")
                                                .hover(|s| s.bg(rgb(0x2995ff)))
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.save_name(window, cx)
                                                })),
                                        ),
                                ),
                        )
                    }),
            )
            .when(self.name_saved, |view| {
                view.child(div().text_size(px(11.)).text_color(rgb(0x30d158)).child(
                    if self.receiver_status.is_ok() {
                        "已保存，设备广播名称已更新"
                    } else {
                        "已保存，等待设备广播更新"
                    },
                ))
            })
            .children(
                self.settings_error
                    .as_ref()
                    .map(|error| self.error_view(error)),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .rounded(px(16.))
                    .bg(rgb(SURFACE))
                    .p(px(16.))
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(div().text_size(px(13.)).child("接收 AirDrop"))
                    .child(div().text_size(px(11.)).text_color(rgb(MUTED)).child(
                        match &self.receiver_status {
                            Ok(()) => "已开启，附近设备可发现此电脑".to_owned(),
                            Err(error) => error.clone(),
                        },
                    ))
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(rgb(MUTED))
                            .child("收到文件时需确认接受。关闭窗口后仍可接收。"),
                    )
                    .child(div().text_size(px(11.)).child("接收文件保存到"))
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(rgb(MUTED))
                            .whitespace_normal()
                            .child(
                                self.download_directory
                                    .as_ref()
                                    .map(|path| path.display().to_string())
                                    .unwrap_or_else(|| "目录不可用，请重新选择".into()),
                            ),
                    )
                    .child(
                        button(
                            "choose-download-directory",
                            if self.picker_open {
                                "选择 / 保存中…"
                            } else {
                                "更改保存目录"
                            },
                            false,
                        )
                        .when(!self.can_change_download_directory(), |button| {
                            button.opacity(0.4)
                        })
                        .on_click(cx.listener(|this, _, _, cx| this.choose_download_directory(cx))),
                    )
                    .when(self.receive.active.is_some(), |card| {
                        card.child(
                            div()
                                .text_size(px(10.))
                                .text_color(rgb(MUTED))
                                .child("接收完成后可更改目录"),
                        )
                    }),
            )
            .into_any_element()
    }

    fn answer_incoming(&mut self, accepted: bool, cx: &mut Context<Self>) {
        if let Some(request) = self.incoming.take() {
            let active = request.active();
            let accepted = accepted && active;
            self.receiver_declined_until = None;
            request
                .accepting
                .store(accepted, std::sync::atomic::Ordering::Release);
            let delivered = request.decision.send(accepted).is_ok();
            if accepted && delivered {
                self.receive.begin(&request.transfer, request.save_dir);
                self.settings_open = false;
                self.firmware.open = false;
            } else if active && delivered {
                self.receive.dismiss_notice();
                self.receiver_declined_until = Some(Instant::now() + Duration::from_secs(3));
            } else {
                self.receive.show_notice(
                    "接收请求已结束，请重新发送",
                    crate::receiving::NoticeKind::Info,
                );
            }
            cx.notify();
        }
    }

    fn receiving_view(&self, compact: bool) -> AnyElement {
        use crate::receiving::Phase as ReceivePhase;
        let transfer = self.receive.active.as_ref().unwrap();
        let (title, detail, fraction) = match transfer.phase {
            ReceivePhase::Waiting => ("等待发送方开始传输", "已接受，请保持设备靠近".into(), None),
            ReceivePhase::Receiving {
                received,
                total: Some(total),
            } if total > 0 => {
                let fraction = (received as f64 / total as f64).clamp(0., 1.) as f32;
                (
                    "正在接收",
                    format!(
                        "{} / {} · {:.0}%",
                        file_size(received),
                        file_size(total),
                        fraction * 100.
                    ),
                    Some(fraction),
                )
            }
            ReceivePhase::Receiving { received, .. } => {
                ("正在接收", format!("已接收 {}", file_size(received)), None)
            }
            ReceivePhase::Confirming => (
                "正在完成接收",
                "正在保存文件并向发送方确认".into(),
                Some(1.),
            ),
        };
        let bar = div().h_full().rounded_full().bg(rgb(BLUE));
        let progress = match fraction {
            Some(fraction) => bar.w(relative(fraction)).into_any_element(),
            None => bar
                .absolute()
                .w(relative(0.25))
                .with_animation(
                    "receiving-indeterminate",
                    Animation::new(Duration::from_secs(2))
                        .repeat()
                        .with_easing(bounce(ease_in_out)),
                    |bar, delta| bar.left(relative(delta * 0.75)),
                )
                .into_any_element(),
        };
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(if compact { 8. } else { 16. }))
            .when(compact, |view| {
                view.flex_shrink_0()
                    .rounded(px(12.))
                    .bg(rgb(SURFACE))
                    .p(px(12.))
            })
            .when(!compact, |view| {
                view.flex_1()
                    .min_h_0()
                    .justify_center()
                    .items_center()
                    .child(
                        div()
                            .size(px(76.))
                            .rounded_full()
                            .bg(rgb(0x233249))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(34.))
                            .text_color(rgb(BLUE))
                            .child("↓"),
                    )
            })
            .child(
                div()
                    .text_size(px(if compact { 13. } else { 22. }))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(rgb(MUTED))
                    .whitespace_normal()
                    .child(format!("{} · {} 个文件", transfer.sender, transfer.files)),
            )
            .child(
                div()
                    .relative()
                    .w_full()
                    .h(px(5.))
                    .rounded_full()
                    .overflow_hidden()
                    .bg(rgb(0x3a3a40))
                    .child(progress),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(rgb(MUTED))
                    .child(detail),
            )
            .when(!compact, |view| {
                view.child(
                    div()
                        .text_size(px(10.))
                        .text_color(rgb(MUTED))
                        .whitespace_normal()
                        .child(format!("保存到 {}", transfer.save_dir.display())),
                )
            })
            .into_any_element()
    }

    fn incoming_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let request = self.incoming.as_ref().unwrap();
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .justify_center()
            .gap(px(16.))
            .child(
                div()
                    .text_size(px(24.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("接收文件"),
            )
            .child(div().text_size(px(13.)).whitespace_normal().child(format!(
                "{} 想向你发送 {} 个文件",
                request.transfer.sender_name,
                request.transfer.files.len()
            )))
            .child(
                div()
                    .id("incoming-files")
                    .max_h(px(160.))
                    .overflow_y_scroll()
                    .text_size(px(12.))
                    .text_color(rgb(MUTED))
                    .children(request.transfer.files.iter().map(|file| {
                        div()
                            .whitespace_normal()
                            .mb(px(6.))
                            .child(file.name.clone())
                    })),
            )
            .child(
                div()
                    .text_size(px(10.))
                    .text_color(rgb(MUTED))
                    .whitespace_normal()
                    .child(format!("接受后保存到 {}", request.save_dir.display())),
            )
            .child(
                button("accept-incoming", "接受", true)
                    .on_click(cx.listener(|this, _, _, cx| this.answer_incoming(true, cx))),
            )
            .child(
                button("decline-incoming", "拒绝", true)
                    .bg(rgb(SURFACE))
                    .on_click(cx.listener(|this, _, _, cx| this.answer_incoming(false, cx))),
            )
            .into_any_element()
    }

    fn empty_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let illustration = div()
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
                    .text_color(rgb(MUTED))
                    .text_center()
                    .child("将文件拖到这里\n发送到附近的 iPhone 或 Mac"),
            )
            .children(
                self.model
                    .error
                    .as_ref()
                    .map(|error| self.error_view(error)),
            );
        let action = button(
            "choose",
            if self.picker_open {
                "正在打开文件选择器…"
            } else if self.model.validating {
                "正在读取文件信息…"
            } else {
                "选择文件"
            },
            !self.picker_open && !self.model.validating,
        )
        .mt(px(8.))
        .flex_shrink_0()
        .w_full()
        .on_click(cx.listener(|this, _, window, cx| this.choose(window, cx)));
        crate::share_layout::empty_share(illustration.into_any_element(), action.into_any_element())
            .into_any_element()
    }

    fn file_previews(&self) -> AnyElement {
        let count = self.model.files.len().min(MAX_PREVIEWS);
        if count == 1 {
            return self.file_tile(0, 44.);
        }
        div()
            .relative()
            .size(px(46.))
            .flex_shrink_0()
            .children((0..count).map(|index| {
                div()
                    .absolute()
                    .left(px((index % 2) as f32 * 24.))
                    .top(px(if count == 2 {
                        12.
                    } else {
                        (index / 2) as f32 * 24.
                    }))
                    .child(self.file_tile(index, 22.))
            }))
            .into_any_element()
    }

    fn file_tile(&self, index: usize, side: f32) -> AnyElement {
        let kind = FileKind::for_path(&self.model.files[index]);
        let tile = div()
            .size(px(side))
            .flex_shrink_0()
            .rounded(px(side * 0.18))
            .overflow_hidden()
            .flex()
            .items_center()
            .justify_center();
        if let Some(Some(image)) = self.model.previews.get(index) {
            return tile
                .bg(rgb(0x323238))
                .child(
                    img(image.clone())
                        .size_full()
                        .object_fit(ObjectFit::Contain),
                )
                .into_any_element();
        }
        // A recognizable document badge is present immediately, including
        // when the associated shell extension cannot produce an icon.
        let (label, color) = kind.badge();
        tile.bg(rgb(color))
            .child(
                div()
                    .w(px(side * 0.66))
                    .h(px(side * 0.78))
                    .rounded(px(2.))
                    .bg(rgb(0xf5f5f7))
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(side * 0.07))
                    .text_color(rgb(color))
                    .font_weight(FontWeight::BOLD)
                    .text_size(px(if label.len() == 1 {
                        side * 0.42
                    } else {
                        side * 0.20
                    }))
                    .child(label)
                    .child(div().w(px(side * 0.36)).h(px(1.)).bg(rgb(color)))
                    .child(div().w(px(side * 0.36)).h(px(1.)).bg(rgb(color))),
            )
            .into_any_element()
    }

    fn files_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let title = if self.model.files.len() == 1 {
            self.model.files[0]
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        } else {
            format!("{} 个文件", self.model.files.len())
        };
        let mut devices = div()
            .id("devices")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(7.));
        if self.model.peers.is_empty() {
            devices = devices.child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .items_center()
                    .gap(px(9.))
                    .text_color(rgb(MUTED))
                    .text_size(px(12.))
                    .child(
                        if self.model.status.is_some() && !self.model.connection_ready() {
                            "无线连接尚未就绪"
                        } else if self.model.scanning {
                            "正在寻找附近设备…"
                        } else {
                            "暂未发现可接收的设备"
                        },
                    )
                    .child(div().text_size(px(10.)).text_center().child(
                        if self.model.status.is_some() && !self.model.connection_ready() {
                            "连接恢复后会自动搜索附近设备"
                        } else {
                            "请让设备保持唤醒，并开启 AirDrop 接收"
                        },
                    )),
            );
        }
        for (index, peer) in self.model.peers.iter().enumerate() {
            let selected = self.model.selected.as_ref() == Some(&peer.service.instance);
            let instance = peer.service.instance.clone();
            devices = devices.child(
                div()
                    .id(("peer", index))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .p(px(12.))
                    .rounded(px(16.))
                    .border_1()
                    .border_color(if selected {
                        rgb(BLUE)
                    } else {
                        rgba(0xffffff12)
                    })
                    .bg(if selected {
                        rgb(0x173353)
                    } else {
                        rgb(SURFACE)
                    })
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.model.toggle_peer(&instance);
                        cx.notify();
                    }))
                    .child(
                        div()
                            .w(px(22.))
                            .h(px(32.))
                            .rounded(px(5.))
                            .border_2()
                            .border_color(rgb(if selected { BLUE } else { MUTED }))
                            .flex()
                            .justify_center()
                            .items_end()
                            .pb(px(3.))
                            .child(div().w(px(8.)).h(px(2.)).rounded_full().bg(rgb(MUTED))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .child(div().truncate().text_size(px(13.)).child(peer.name.clone()))
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(9.))
                                    .text_color(rgb(MUTED))
                                    .child(format!(
                                        "AirDrop · {}",
                                        peer.service.instance.split('.').next().unwrap_or("")
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .text_color(rgb(BLUE))
                            .child(if selected { "✓" } else { "" }),
                    ),
            );
        }
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap(px(14.))
            .child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .items_start()
                    .gap(px(10.))
                    .p(px(12.))
                    .rounded(px(16.))
                    .bg(rgb(SURFACE))
                    .child(self.file_previews())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .child(
                                div()
                                    .id("file-name")
                                    .w_full()
                                    .max_h(px(96.))
                                    .overflow_y_scroll()
                                    .whitespace_normal()
                                    .text_size(px(12.))
                                    .line_height(px(16.))
                                    .child(title),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap(px(8.))
                                    .text_size(px(10.))
                                    .child(
                                        div()
                                            .text_color(rgb(MUTED))
                                            .child(file_size(self.model.bytes)),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .flex_shrink_0()
                                            .items_center()
                                            .gap(px(12.))
                                            .child(
                                                div()
                                                    .id("open-file")
                                                    .text_color(rgb(if self.can_open_files() {
                                                        BLUE
                                                    } else {
                                                        MUTED
                                                    }))
                                                    .when(self.can_open_files(), |button| {
                                                        button.cursor_pointer()
                                                    })
                                                    .child(if self.opening_files.is_some() {
                                                        "打开中…"
                                                    } else if self.model.files.len() > 1 {
                                                        "全部打开"
                                                    } else {
                                                        "打开文件"
                                                    })
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        this.open_files(cx)
                                                    })),
                                            )
                                            .child(
                                                div()
                                                    .id("replace")
                                                    .cursor_pointer()
                                                    .text_color(rgb(BLUE))
                                                    .child(if self.picker_open {
                                                        "选择中…"
                                                    } else if self.model.validating {
                                                        "读取中…"
                                                    } else {
                                                        "更换文件"
                                                    })
                                                    .on_click(cx.listener(
                                                        |this, _, window, cx| {
                                                            this.choose(window, cx)
                                                        },
                                                    )),
                                            ),
                                    ),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .justify_between()
                    .text_size(px(11.))
                    .text_color(rgb(MUTED))
                    .child("发送到附近设备")
                    .child(
                        div()
                            .id("refresh")
                            .cursor_pointer()
                            .text_color(rgb(BLUE))
                            .child(if self.model.scanning {
                                if self.model.peers.is_empty() {
                                    "搜索中…"
                                } else {
                                    "搜索其他设备…"
                                }
                            } else {
                                "重新搜索"
                            })
                            .on_click(cx.listener(|this, _, _, cx| this.scan(cx))),
                    ),
            )
            .child(devices)
            .children(
                self.model
                    .error
                    .as_ref()
                    .map(|error| self.error_view(error)),
            )
            .child(
                button(
                    "send",
                    if self.model.selected.is_some() {
                        "发送"
                    } else {
                        "选择一台设备"
                    },
                    self.model.can_send(),
                )
                .on_click(cx.listener(|this, _, _, cx| this.send(cx))),
            )
            .into_any_element()
    }

    fn transfer_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let complete = self.model.phase == Phase::Complete;
        let declined = self.model.phase == Phase::Declined;
        let subtitle = match self.model.phase {
            Phase::Preparing => "正在整理文件",
            Phase::Waiting => "请在接收设备上轻点「接受」",
            Phase::Confirming => "文件已上传，正在等待对方确认",
            Phase::Complete => "文件已被接收设备确认",
            Phase::Cancelled => "本次传输已停止",
            Phase::Declined => "接收方拒绝了本次传输\n文件已保留，可返回重新选择设备",
            Phase::Cancelling => "正在关闭传输连接",
            Phase::Failed => "文件已保留，可以重新尝试",
            _ => "请保持设备靠近",
        };
        let mut view = div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(14.))
            .child(
                div()
                    .size(px(76.))
                    .rounded_full()
                    .bg(rgb(if complete {
                        0x203b2b
                    } else if declined {
                        SURFACE
                    } else {
                        0x233249
                    }))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(34.))
                    .text_color(rgb(if complete {
                        0x30d158
                    } else if declined {
                        MUTED
                    } else {
                        BLUE
                    }))
                    .child(if complete {
                        "✓"
                    } else if declined {
                        "×"
                    } else {
                        "↑"
                    }),
            )
            .child(
                div()
                    .text_size(px(22.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(self.model.phase.title()),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(rgb(MUTED))
                    .child(self.model.target.clone()),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(rgb(MUTED))
                    .text_center()
                    .child(subtitle),
            );
        if let Phase::Uploading { sent, total } = self.model.phase {
            let fraction = if total == 0 {
                0.0
            } else {
                (sent as f32 / total as f32).min(1.0)
            };
            view = view
                .child(
                    div()
                        .w_full()
                        .h(px(5.))
                        .rounded_full()
                        .bg(rgb(0x3a3a40))
                        .child(
                            div()
                                .h_full()
                                .w(relative(fraction))
                                .rounded_full()
                                .bg(rgb(BLUE)),
                        ),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(rgb(MUTED))
                        .child(format!(
                            "{} / {} · {:.0}%",
                            file_size(sent),
                            file_size(total),
                            fraction * 100.
                        )),
                );
        }
        view = view.children(
            self.model
                .error
                .as_ref()
                .map(|error| self.error_view(error)),
        );
        if self.model.phase.busy() {
            view = view.child(
                button(
                    "cancel",
                    if self.model.phase == Phase::Cancelling {
                        "正在取消…"
                    } else {
                        "取消发送"
                    },
                    false,
                )
                .w_full()
                .mt(px(12.))
                .on_click(cx.listener(|this, _, _, cx| {
                    if this.model.phase != Phase::Cancelling {
                        this.cancel(cx);
                    }
                })),
            );
        } else {
            view = view.child(
                button("done", if complete { "完成" } else { "返回" }, true)
                    .w_full()
                    .mt(px(12.))
                    .on_click(cx.listener(|this, _, window, cx| {
                        if this.model.phase == Phase::Complete {
                            this.reset(window, cx);
                        } else {
                            this.model.phase = Phase::Ready;
                            this.model.error = None;
                            cx.notify();
                        }
                    })),
            );
        }
        view.into_any_element()
    }
    fn error_view(&self, error: &str) -> AnyElement {
        div()
            .id("error")
            .flex_shrink_0()
            .w_full()
            .max_h(px(65.))
            .overflow_y_scroll()
            .text_size(px(10.))
            .text_color(rgb(0xffb56b))
            .child(error.to_string())
            .into_any_element()
    }
}
impl Drop for Capsule {
    fn drop(&mut self) {
        if let Some(cancel) = &self.cancel {
            cancel.cancel();
        }
        if let Some(cancel) = &self.scan_cancel {
            cancel.cancel();
        }
        if let Some(cancel) = &self.firmware_cancel {
            cancel.cancel();
        }
    }
}
impl Render for Capsule {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let show_orb = self.visibility == Visibility::Share
            && self.exit_state.is_none()
            && self.incoming.is_none()
            && self.receive.active.is_none()
            && !self.firmware.open
            && !self.settings_open
            && self.model.phase == Phase::Ready
            && self.model.files.is_empty();
        self.orb.update(show_orb, self.model.connection_ready());
        div()
            .id("root")
            .size_full()
            .font_family("Segoe UI")
            .text_color(rgb(INK))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if this.exit_state.is_some() {
                    return;
                }
                if event.keystroke.modifiers.control && event.keystroke.key == "q" {
                    this.quit(window, cx);
                    return;
                }
                if this.name_input.is_some() {
                    return;
                }
                if event.keystroke.key == "escape" {
                    if this.settings_open || this.firmware.open {
                        this.back(window, cx);
                    } else {
                        this.expand(false, window, cx);
                    }
                }
                if !this.settings_open
                    && event.keystroke.modifiers.control
                    && event.keystroke.key == "o"
                {
                    this.choose(window, cx);
                }
            }))
            .child(if self.exit_state.is_some() {
                self.exit_panel(cx)
            } else if self.visibility == Visibility::Share {
                self.panel(cx)
            } else {
                self.pill()
            })
    }
}
fn settings_gear() -> impl IntoElement {
    canvas(
        |_, _, _| (),
        |bounds, _, window, _| {
            let center = bounds.origin + point(px(8.), px(8.));
            let mut gear = PathBuilder::stroke(px(1.25));
            for tooth in 0..8 {
                let angle = tooth as f32 * std::f32::consts::TAU / 8.;
                for (index, (offset, radius)) in
                    [(-0.16, 5.1), (-0.16, 6.6), (0.16, 6.6), (0.16, 5.1)]
                        .into_iter()
                        .enumerate()
                {
                    let angle = angle + offset;
                    let position =
                        center + point(px(angle.cos() * radius), px(angle.sin() * radius));
                    if tooth == 0 && index == 0 {
                        gear.move_to(position);
                    } else {
                        gear.line_to(position);
                    }
                }
            }
            gear.close();
            for index in 0..24 {
                let angle = index as f32 * std::f32::consts::TAU / 24.;
                let position = center + point(px(angle.cos() * 2.2), px(angle.sin() * 2.2));
                if index == 0 {
                    gear.move_to(position);
                } else {
                    gear.line_to(position);
                }
            }
            gear.close();
            if let Ok(path) = gear.build() {
                window.paint_path(path, window.text_style().color);
            }
        },
    )
    .size(px(16.))
    .flex_shrink_0()
}

fn back_chevron() -> impl IntoElement {
    canvas(
        |_, _, _| (),
        |bounds, _, window, _| {
            let mut path = PathBuilder::stroke(px(1.5));
            path.move_to(bounds.origin + point(px(8.5), px(3.)));
            path.line_to(bounds.origin + point(px(4.5), px(7.)));
            path.line_to(bounds.origin + point(px(8.5), px(11.)));
            if let Ok(path) = path.build() {
                let color = window.text_style().color;
                window.paint_path(path, color);
            }
        },
    )
    .size(px(14.))
    .flex_shrink_0()
}

fn firmware_card() -> Div {
    div()
        .w_full()
        .flex_shrink_0()
        .rounded(px(12.))
        .border_1()
        .border_color(rgba(0xffffff12))
        .bg(rgb(SURFACE))
        .p(px(12.))
        .flex()
        .flex_col()
        .gap(px(8.))
}

fn firmware_hint(text: impl Into<SharedString>, color: u32) -> Div {
    div()
        .text_size(px(11.))
        .line_height(px(16.))
        .text_color(rgb(color))
        .whitespace_normal()
        .child(text.into())
}

fn firmware_button(id: &'static str, label: &'static str, primary: bool) -> Stateful<Div> {
    button(id, label, primary)
        .h(px(36.))
        .w_full()
        .flex_shrink_0()
        .rounded(px(10.))
        .text_size(px(12.))
}

fn button(id: &'static str, label: &'static str, primary: bool) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(42.))
        .rounded(px(13.))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .text_size(px(13.))
        .font_weight(FontWeight::MEDIUM)
        .bg(rgb(if primary { BLUE } else { 0x303037 }))
        .text_color(rgb(if primary { 0xffffff } else { MUTED }))
        .child(label)
        .hover(move |s| s.bg(rgb(if primary { 0x2592ff } else { 0x3b3b43 })))
}
