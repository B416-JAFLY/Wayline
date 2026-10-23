use crate::{
    backend::{self, Event},
    i18n::{self, Language},
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
    FlushingHistory(bool),
    Failed(String),
}

pub struct Capsule {
    model: Model,
    orb: crate::orb::ConnectionOrb,
    language: Language,
    visibility: Visibility,
    shell: Option<Shell>,
    focus: FocusHandle,
    tx: mpsc::Sender<Event>,
    cancel: Option<SendCancellation>,
    scan_cancel: Option<SendCancellation>,
    workers: Vec<std::thread::JoinHandle<()>>,
    history_thread: Option<std::thread::JoinHandle<()>>,
    firmware: crate::firmware::Model,
    firmware_cancel: Option<omdrop_flash::Cancellation>,
    firmware_worker: Option<std::thread::JoinHandle<()>>,
    picker_open: bool,
    name_input: Option<Entity<crate::name_input::NameInput>>,
    name_input_subscription: Option<Subscription>,
    settings_open: bool,
    history_open: bool,
    history: Vec<omdrop_host::TransferHistoryRecord>,
    history_loaded: bool,
    history_error: Option<String>,
    history_clear_until: Option<Instant>,
    history_worker: Option<crate::history::StoreWorker>,
    language_menu_open: bool,
    settings_error: Option<String>,
    radio_channel_pending: Option<u16>,
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
        let detected = Language::detect();
        let (language, language_error) = match Language::load_preference() {
            Ok(Some(language)) => (language, None),
            Ok(None) => (detected, None),
            Err(error) => (detected, Some(error)),
        };
        i18n::set_current(language);
        let text = language.strings();
        let (device_name, mut settings_error) = match crate::settings::load() {
            Ok(name) => (name, None),
            Err(error) => (
                crate::settings::default_name(),
                Some(format!("{}{error:#}", text.err_device_name_load)),
            ),
        };
        if let Some(error) = language_error {
            settings_error = Some(format!("{}{error}", text.err_language_load));
        }
        let download_directory = match crate::receiving::save_directory() {
            Ok(directory) => Some(directory),
            Err(error) => {
                settings_error = Some(format!("{}{error:#}", text.err_download_dir_load));
                None
            }
        };
        let (tx, rx) = mpsc::channel();
        let (history_worker, history_thread) = crate::history::StoreWorker::start(tx.clone());
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
            language,
            visibility: Visibility::Hidden,
            shell,
            focus: cx.focus_handle(),
            tx,
            cancel: None,
            scan_cancel: None,
            workers: Vec::new(),
            history_thread: Some(history_thread),
            firmware: crate::firmware::Model::default(),
            firmware_cancel: None,
            firmware_worker: None,
            picker_open: false,
            name_input: None,
            name_input_subscription: None,
            settings_open: false,
            history_open: false,
            history: Vec::new(),
            history_loaded: false,
            history_error: None,
            history_clear_until: None,
            history_worker: Some(history_worker),
            language_menu_open: false,
            settings_error,
            radio_channel_pending: None,
            name_saved: false,
            device_name,
            receiver,
            monitor: Some(monitor),
            exit_state: None,
            receiver_status: Err(text.receiver_starting.into()),
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
            let mut shutdown_result = None;
            for event in events {
                match event {
                    Event::ShutdownDone(Ok(background_running)) => {
                        shutdown_result = Some(background_running);
                        continue;
                    }
                    Event::HistoryFlushed => {
                        let background_running =
                            matches!(self.exit_state, Some(ExitState::FlushingHistory(true)));
                        if let Some(shell) = &self.shell
                            && let Some(error) = &self.history_error
                        {
                            shell.notify(self.language.strings().history_title, error);
                        } else if background_running && let Some(shell) = &self.shell {
                            let text = self.language.strings();
                            shell.notify(text.app_exited_title, text.app_exited_detail);
                        }
                        cx.quit();
                        return;
                    }
                    Event::SendDone(id, result) => {
                        if let Some(record) = self.model.take_history_record(
                            id,
                            &result,
                            Instant::now(),
                            crate::history::unix_time_ms(std::time::SystemTime::now()),
                        ) && let Some(worker) = &self.history_worker
                        {
                            worker.append(record, Vec::new());
                        }
                        let _ = self.model.apply(Event::SendDone(id, result));
                    }
                    Event::Transfer(id, event) => {
                        let _ = self.model.apply(Event::Transfer(id, event));
                    }
                    Event::ReceiverActivity(event) => {
                        let record = self.receive.history_record_for_event(
                            &event,
                            Instant::now(),
                            crate::history::unix_time_ms(std::time::SystemTime::now()),
                        );
                        let received_files = match &event {
                            omdrop_core::ReceiverEvent::Received { files, .. } => files.clone(),
                            _ => Vec::new(),
                        };
                        if self.receive.apply(&event)
                            && let Some(record) = record
                            && let Some(worker) = &self.history_worker
                        {
                            worker.append(record, received_files);
                        }
                    }
                    Event::HistoryUpdated(Err(error)) => {
                        self.history_error = Some(format!(
                            "{}{error}",
                            self.language.strings().history_save_error
                        ));
                    }
                    Event::HistoryUpdated(Ok(_)) => self.history_error = None,
                    Event::HistoryCleared(Err(error)) => {
                        self.history_error = Some(format!(
                            "{}{error}",
                            self.language.strings().history_clear_error
                        ));
                    }
                    Event::HistoryCleared(Ok(_)) => self.history_error = None,
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
            if let Some(background_running) = shutdown_result {
                self.flush_history_and_quit(background_running);
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
                Event::ShellError(error) => {
                    self.model.error = Some(error);
                    changed = true;
                    continue;
                }
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
                Event::HistoryLoaded(result) => {
                    self.history_loaded = true;
                    match result {
                        Ok(records) => {
                            self.history = records;
                            self.history_error = None;
                        }
                        Err(error) => {
                            self.history_error = Some(format!(
                                "{}{error}",
                                self.language.strings().history_load_error
                            ));
                        }
                    }
                    changed = true;
                    continue;
                }
                Event::HistoryUpdated(result) => {
                    match result {
                        Ok(records) => {
                            self.history = records;
                            self.history_error = None;
                        }
                        Err(error) => {
                            self.history_error = Some(format!(
                                "{}{error}",
                                self.language.strings().history_save_error
                            ));
                        }
                    }
                    changed = true;
                    continue;
                }
                Event::HistoryCleared(result) => {
                    match result {
                        Ok(records) => {
                            self.history = records;
                            self.history_error = None;
                        }
                        Err(error) => {
                            self.history_error = Some(format!(
                                "{}{error}",
                                self.language.strings().history_clear_error
                            ));
                        }
                    }
                    self.history_clear_until = None;
                    changed = true;
                    continue;
                }
                Event::ReceiverStatus(status) => {
                    if let Err(error) = &status {
                        self.receive.interrupt_with_details(
                            self.language.strings().receive_interrupted.into(),
                            error.clone(),
                        );
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
                                self.receiver_status =
                                    Err(self.language.strings().receiver_updating_dir.into());
                                self.receiver.reload_directory();
                            }
                        }
                        Err(error) => {
                            self.settings_error = Some(format!(
                                "{}{error}",
                                self.language.strings().err_download_dir_save
                            ))
                        }
                    }
                    changed = true;
                    continue;
                }
                Event::RadioChannelChanged(result) => {
                    self.radio_channel_pending = None;
                    self.receiver = crate::receiving::ReceiverHost::new(self.tx.clone());
                    self.receiver.configure(
                        self.model
                            .connection_ready()
                            .then(|| self.device_name.clone()),
                    );
                    match result {
                        Ok(_) => self.settings_error = None,
                        Err(error) => {
                            self.settings_error = Some(format!(
                                "{}{error}",
                                self.language.strings().err_channel_change
                            ));
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
                        || self.history_open
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
                    let finished_at = Instant::now();
                    let record = self.receive.history_record_for_event(
                        &event,
                        finished_at,
                        crate::history::unix_time_ms(std::time::SystemTime::now()),
                    );
                    let received_files = match &event {
                        omdrop_core::ReceiverEvent::Received { files, .. } => files.clone(),
                        _ => Vec::new(),
                    };
                    let applied = self.receive.apply(&event);
                    if applied
                        && let Some(record) = record
                        && let Some(worker) = &self.history_worker
                    {
                        worker.append(record, received_files);
                    }
                    if applied
                        && matches!(event, omdrop_core::ReceiverEvent::Received { .. })
                        && let (Some(shell), Some(message)) = (&self.shell, &self.receive.notice)
                    {
                        shell.notify(
                            self.language.strings().notice_files_received,
                            &message.message,
                        );
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
                        changed |= self
                            .receive
                            .interrupt(self.language.strings().connection_lost_receive.into());
                    }
                }
                _ => {}
            }
            let files_ready = matches!(&event, Event::FilesValidated(id, _, Ok(_)) if *id == self.model.selection_id);
            if files_ready {
                self.clear_previews(window);
            }
            let send_record = if let Event::SendDone(id, result) = &event {
                self.model.take_history_record(
                    *id,
                    result,
                    Instant::now(),
                    crate::history::unix_time_ms(std::time::SystemTime::now()),
                )
            } else {
                None
            };
            changed |= self.model.apply(event);
            if let Some(record) = send_record
                && let Some(worker) = &self.history_worker
            {
                worker.append(record, Vec::new());
            }
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
                            self.model.error = Some(format!(
                                "{}{error}",
                                self.language.strings().err_file_picker
                            ))
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
                Action::HistoryFolderOpened(result) => {
                    if let Err(error) = result {
                        self.history_error = Some(format!(
                            "{}{error}",
                            self.language.strings().history_open_error
                        ));
                    }
                    changed = true;
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
                self.language.strings().incoming_ended,
                crate::receiving::NoticeKind::Info,
            );
            changed = true;
        }
        let now = Instant::now();
        changed |= self.receive.expire_notice(now);
        changed |= self.model.expire_speed(now);
        changed |= self.receive.expire_speed(now);
        if self
            .history_clear_until
            .is_some_and(|deadline| now >= deadline)
        {
            self.history_clear_until = None;
            changed = true;
        }
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
        if matches!(
            self.exit_state,
            Some(ExitState::Stopping | ExitState::FlushingHistory(_))
        ) {
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
        self.history_open = false;
        self.language_menu_open = false;
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

    fn flush_history_and_quit(&mut self, background_running: bool) {
        let Some(history) = self.history_worker.take() else {
            self.exit_state = Some(ExitState::FlushingHistory(background_running));
            let _ = self.tx.send(Event::HistoryFlushed);
            return;
        };
        self.exit_state = Some(ExitState::FlushingHistory(background_running));
        let flush = history.flush_handle();
        drop(history);
        let history_thread = self.history_thread.take();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let _ = flush.join();
            if let Some(worker) = history_thread {
                let _ = worker.join();
            }
            let _ = tx.send(Event::HistoryFlushed);
        });
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
        self.history_open = false;
        self.history_clear_until = None;
        self.language_menu_open = false;
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
                Err(error) => {
                    self.model.error =
                        Some(format!("{}{error}", self.language.strings().err_open_file))
                }
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
            self.model.error = Some(format!(
                "{}{error}",
                self.language.strings().err_file_picker
            ));
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

    fn can_change_radio_channel(&self, channel: u16) -> bool {
        if self.radio_channel_pending.is_some()
            || self.firmware.busy
            || self.model.phase.busy()
            || self.picker_open
            || self.name_input.is_some()
            || self.incoming.is_some()
            || self.receive.active.is_some()
            || self.receiver.has_active_transfers()
        {
            return false;
        }
        let Some(Ok(status)) = self.model.status.as_ref() else {
            return false;
        };
        if status.channel == channel {
            return false;
        }
        match channel {
            6 => true,
            44 | 149 => status.target.as_deref() == Some("esp32c5") && status.ready(),
            _ => false,
        }
    }

    fn set_radio_channel(&mut self, channel: u16, cx: &mut Context<Self>) {
        if !self.can_change_radio_channel(channel) {
            return;
        }
        self.radio_channel_pending = Some(channel);
        self.settings_error = None;
        self.receiver_status = Err(self.language.strings().switching_channel.into());
        let receiver_worker = self.receiver.shutdown();
        self.workers.push(backend::change_radio_channel(
            channel,
            receiver_worker,
            self.tx.clone(),
        ));
        cx.notify();
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
                Err(error) => {
                    self.settings_error = Some(format!(
                        "{}{error:#}",
                        self.language.strings().err_receive_dir_picker
                    ))
                }
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
                    self.receiver_status = Err(self.language.strings().updating_broadcast.into());
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
            Err(error) => {
                self.settings_error =
                    Some(error.downcast_ref::<omdrop_host::NameError>().map_or_else(
                        || format!("{}{error:#}", self.language.strings().err_saving_name),
                        |kind| self.language.name_error(*kind),
                    ))
            }
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
        self.history_open = false;
        self.language_menu_open = false;
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
        } else if self.history_open {
            self.history_open = false;
            self.history_clear_until = None;
            self.focus.focus(window);
            cx.notify();
        } else if self.settings_open {
            self.settings_open = false;
            self.language_menu_open = false;
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
            self.model.error = Some(self.language.strings().busy_device_add_files.into());
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

    fn open_history(&mut self, cx: &mut Context<Self>) {
        if self.settings_open && !self.firmware.busy && self.receive.active.is_none() {
            self.history_open = true;
            self.history_clear_until = None;
            cx.notify();
        }
    }

    fn clear_history(&mut self, cx: &mut Context<Self>) {
        if self.history.is_empty() && self.history_error.is_none() {
            return;
        }
        if self.history_clear_until.is_some() {
            if let Some(worker) = &self.history_worker {
                worker.clear();
            }
            self.history_clear_until = None;
            self.history_error = None;
        } else {
            self.history_clear_until = Some(Instant::now() + Duration::from_secs(5));
        }
        cx.notify();
    }

    fn cancel_clear_history(&mut self, cx: &mut Context<Self>) {
        self.history_clear_until = None;
        cx.notify();
    }

    fn open_history_folder(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(shell) = &self.shell else {
            return;
        };
        if let Err(error) = shell.open_history_folder(path) {
            self.history_error = Some(format!(
                "{}{error:#}",
                self.language.strings().history_open_error
            ));
        }
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
        let text = self.language.strings();
        match &self.model.status {
            Some(Ok(status)) if status.ready() => (text.status_connected, 0x30d158),
            Some(Ok(_)) => (text.status_waiting_bridge, 0xff9f0a),
            Some(Err(_)) => (text.status_service_unavailable, 0xff9f0a),
            None => (text.status_connecting, MUTED),
        }
    }

    fn pill(&self) -> AnyElement {
        let text = self.language.strings();
        let subtitle = if self.model.phase.busy() {
            self.model.phase.title(self.language)
        } else if self.model.phase == Phase::Complete {
            text.pill_delivered
        } else {
            text.pill_drop_to_send
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
        let text = self.language.strings();
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
                        text.quit_failed_title
                    } else {
                        text.quitting_title
                    }),
            )
            .child(
                div()
                    .text_size(px(13.))
                    .text_color(rgb(MUTED))
                    .child(error.cloned().unwrap_or_else(|| text.quit_detail.into())),
            )
            .when(error.is_some(), |panel| {
                panel.child(
                    button("retry-quit", text.retry_quit, true)
                        .on_click(cx.listener(|this, _, window, cx| this.quit(window, cx))),
                )
            })
            .into_any_element()
    }

    fn panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let text = self.language.strings();
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
                                            .child(text.firmware_title)
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
                                            if self.history_open {
                                                text.back
                                            } else if self.settings_open || self.firmware.open {
                                                text.back_to_share
                                            } else {
                                                text.back
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
                                            .child(if self.history_open {
                                                text.history_title
                                            } else if self.firmware.open {
                                                text.firmware_title
                                            } else {
                                                text.settings_title
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
                        if self.history_open {
                            self.history_view(cx)
                        } else {
                            self.settings_view(cx)
                        }
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
                                || self.history_open
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
                                copy_label: text.copy,
                                copied_label: text.copied,
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
                                .child(text.receiver_declined),
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
                        text.footer_save
                    } else if self.settings_open || self.history_open || self.firmware.open {
                        text.footer_back
                    } else if self.picker_open {
                        text.footer_choosing
                    } else if self.model.validating {
                        text.footer_reading
                    } else {
                        text.footer_choose
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
        self.history_open = false;
        self.language_menu_open = false;
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
        let board = self.firmware.package.as_ref().unwrap().board.clone();
        let paths = omdrop_flash::installed_manifest_for_board(&board)
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
        self.firmware.message = self.language.strings().firmware_prepare_maintenance.into();
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
        let text = self.language.strings();
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
                                || text.firmware_install.to_string(),
                                |package| {
                                    omdrop_flash::profile_for_board(&package.board).map_or_else(
                                        || package.board.clone(),
                                        |profile| profile.label.into(),
                                    )
                                },
                            )),
                    )
                    .child(firmware_hint(
                        self.firmware.package.as_ref().map_or_else(
                            || text.firmware_not_found.to_string(),
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
                        self.language
                            .firmware_package_version(self.firmware.version.as_deref()),
                        MUTED,
                    )),
            );
        if self.firmware.packages.len() > 1 {
            let mut profiles = div().flex().flex_wrap().gap(px(6.));
            for (index, package) in self.firmware.packages.iter().enumerate() {
                let board = package.board.clone();
                let selected = self
                    .firmware
                    .package
                    .as_ref()
                    .is_some_and(|current| current.board == board);
                let label = omdrop_flash::profile_for_board(&board)
                    .map_or_else(|| board.clone(), |profile| profile.label.into());
                profiles = profiles.child(
                    div()
                        .id(("firmware-profile", index))
                        .h(px(32.))
                        .px(px(10.))
                        .rounded(px(7.))
                        .border_1()
                        .border_color(rgb(if selected { BLUE } else { 0x3a3a40 }))
                        .bg(rgb(if selected { 0x163353 } else { 0x202024 }))
                        .flex()
                        .items_center()
                        .text_size(px(10.))
                        .text_color(rgb(if selected { INK } else { MUTED }))
                        .child(label)
                        .when(self.firmware.busy, |button| button.opacity(0.4))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if this.firmware.select_profile(&board) {
                                cx.notify();
                            }
                        })),
                );
            }
            body = body.child(
                firmware_card().child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .child(
                            div()
                                .text_size(px(12.))
                                .font_weight(FontWeight::MEDIUM)
                                .child(text.firmware_target),
                        )
                        .child(profiles),
                ),
            );
        }
        if self.firmware.confirm_exit && self.firmware.busy {
            body = body.child(
                firmware_card()
                    .child(
                        div()
                            .text_size(px(12.))
                            .font_weight(FontWeight::MEDIUM)
                            .child(text.firmware_stop_quit),
                    )
                    .child(firmware_hint(text.firmware_stop_quit_detail, 0xffb56b)),
            );
            let actions = div()
                .flex()
                .gap(px(8.))
                .child(div().flex_1().min_w_0().child(
                    firmware_button("keep-flashing", text.firmware_keep_install, true).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.firmware.confirm_exit = false;
                            cx.notify();
                        }),
                    ),
                ))
                .child(
                    div().flex_1().min_w_0().child(
                        firmware_button("cancel-flash-quit", text.firmware_cancel_quit, false)
                            .on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.begin_quit(true, window, cx)
                                }),
                            ),
                    ),
                );
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
            body = body
                .child(status)
                .child(firmware_hint(text.firmware_write_warning, 0xffb56b));
            let action = firmware_button(
                "cancel-firmware",
                if self.firmware.cancelling {
                    text.firmware_stopping
                } else {
                    text.firmware_cancel
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
                        .child(text.firmware_connect),
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
                            text.firmware_checking
                        } else {
                            text.firmware_refresh
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
                    text.firmware_searching
                } else {
                    text.firmware_no_port
                },
                MUTED,
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
        if self.firmware.selected.is_some()
            && self.firmware.package.is_some()
            && !self.firmware.profile_matches_selected_port(
                self.firmware.package.as_ref().map(|p| p.board.as_str()),
            )
        {
            body = body.child(firmware_hint(text.firmware_profile_mismatch, 0xffb56b));
        }
        let upgrade = self.firmware.mode == omdrop_flash::InstallMode::Upgrade;
        let mut mode_buttons = div().flex().gap(px(6.));
        for (id, label, mode) in [
            (
                "firmware-upgrade",
                text.firmware_keep_settings,
                omdrop_flash::InstallMode::Upgrade,
            ),
            (
                "firmware-reinstall",
                text.firmware_reinstall,
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
                        .child(text.firmware_options),
                )
                .child(mode_buttons)
                .child(firmware_hint(
                    if upgrade {
                        text.firmware_keep_hint
                    } else {
                        text.firmware_replace_hint
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
                                .child(text.firmware_confirm_board),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            if !this.firmware.busy {
                                this.firmware.board_confirmed = !this.firmware.board_confirmed;
                                cx.notify();
                            }
                        })),
                )
                .child(firmware_hint(text.firmware_chip_hint, MUTED)),
        );
        if transfer_active {
            body = body.child(firmware_hint(text.firmware_transfer_active, 0xffb56b));
        }
        let action = firmware_button(
            "install-firmware",
            if self.firmware.error.is_some() {
                text.firmware_retry
            } else if upgrade {
                text.firmware_start_update
            } else {
                text.firmware_start_install
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

    fn radio_settings_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let text = self.language.strings();
        let status = self
            .model
            .status
            .as_ref()
            .and_then(|status| status.as_ref().ok());
        let channel = status.map(|status| status.channel);
        let target = status.and_then(|status| status.target.as_deref());
        let target_label = match target {
            Some("esp32s3") => "ESP32-S3",
            Some("esp32c5") => "ESP32-C5",
            _ => text.radio_unknown_board,
        };
        let current_label = channel.map(|channel| match channel {
            6 => text.channel_6,
            44 => text.channel_44,
            149 => text.channel_149,
            _ => text.channel_unknown,
        });
        let status_text = match (status, current_label) {
            (Some(status), Some(current)) if status.ready() => {
                format!("{target_label} · {} {current}", text.radio_current)
            }
            (Some(_), Some(current)) => format!(
                "{target_label} · {} {current} · {}",
                text.radio_configured, text.radio_not_ready
            ),
            (Some(_), None) => {
                format!("{target_label} · {}", text.radio_current_unknown)
            }
            _ => text.radio_reading.into(),
        };
        let choices: &[u16] = match target {
            Some("esp32c5") => &[6, 44, 149],
            Some("esp32s3") | None => &[6],
            _ => &[6],
        };
        div()
            .flex_shrink_0()
            .rounded(px(16.))
            .bg(rgb(SURFACE))
            .p(px(16.))
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(div().text_size(px(13.)).child(text.radio_title))
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(rgb(MUTED))
                    .child(status_text),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(8.))
                    .children(choices.iter().copied().map(|channel| {
                        let selected = channel == status.map_or(0, |status| status.channel);
                        let enabled = self.can_change_radio_channel(channel);
                        let label = match channel {
                            6 => text.channel_6,
                            44 => text.channel_44,
                            _ => text.channel_149,
                        };
                        div()
                            .id(("radio-channel", channel as u32))
                            .h(px(34.))
                            .px(px(12.))
                            .rounded(px(8.))
                            .bg(rgb(if selected { BLUE } else { 0x393940 }))
                            .flex()
                            .items_center()
                            .text_size(px(11.))
                            .font_weight(FontWeight::MEDIUM)
                            .child(label)
                            .when(!enabled, |button| {
                                button
                                    .opacity(if selected { 0.65 } else { 0.4 })
                                    .cursor(CursorStyle::Arrow)
                            })
                            .when(enabled, |button| {
                                button
                                    .cursor_pointer()
                                    .hover(|style| style.bg(rgb(0x484850)))
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_radio_channel(channel, cx)
                            }))
                    })),
            )
            .when(self.radio_channel_pending.is_some(), |view| {
                view.child(
                    div()
                        .text_size(px(10.))
                        .text_color(rgb(MUTED))
                        .child(text.radio_restarting),
                )
            })
            .into_any_element()
    }

    fn language_settings_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let text = self.language.strings();
        div()
            .flex_shrink_0()
            .rounded(px(16.))
            .bg(rgb(SURFACE))
            .p(px(16.))
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(div().text_size(px(13.)).child(text.language_title))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                        if this.language_menu_open {
                            this.language_menu_open = false;
                            cx.notify();
                        }
                    }))
                    .child(
                        div()
                            .id("language-select")
                            .h(px(36.))
                            .w_full()
                            .px(px(12.))
                            .rounded(px(9.))
                            .border_1()
                            .border_color(rgb(if self.language_menu_open {
                                BLUE
                            } else {
                                0x3a3a40
                            }))
                            .bg(rgb(0x202024))
                            .flex()
                            .items_center()
                            .justify_between()
                            .cursor_pointer()
                            .text_size(px(12.))
                            .hover(|style| style.bg(rgb(0x2a2a31)))
                            .child(self.language.label())
                            .child(dropdown_chevron(self.language_menu_open))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.language_menu_open = !this.language_menu_open;
                                cx.notify();
                            })),
                    )
                    .when(self.language_menu_open, |field| {
                        field.child(
                            div()
                                .id("language-menu")
                                .w_full()
                                .max_h(px(240.))
                                .overflow_y_scroll()
                                .rounded(px(10.))
                                .border_1()
                                .border_color(rgba(0xffffff18))
                                .bg(rgb(0x1c1c1f))
                                .p(px(4.))
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                .children(Language::ALL.into_iter().map(|language| {
                                    let selected = self.language == language;
                                    div()
                                        .id(("language-option", language as u32))
                                        .h(px(30.))
                                        .px(px(8.))
                                        .rounded(px(6.))
                                        .bg(rgb(if selected { 0x163353 } else { 0x1c1c1f }))
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .cursor_pointer()
                                        .text_size(px(11.))
                                        .text_color(rgb(if selected { INK } else { MUTED }))
                                        .hover(|style| style.bg(rgb(0x2f2f36)))
                                        .child(language.label())
                                        .when(selected, |row| {
                                            row.child(div().text_color(rgb(BLUE)).child("✓"))
                                        })
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.set_language(language, cx)
                                        }))
                                })),
                        )
                    }),
            )
            .child(
                div()
                    .text_size(px(10.))
                    .text_color(rgb(MUTED))
                    .child(text.language_hint),
            )
            .into_any_element()
    }

    fn set_language(&mut self, language: Language, cx: &mut Context<Self>) {
        self.language_menu_open = false;
        if self.language == language {
            cx.notify();
            return;
        }
        let result = crate::settings::path()
            .and_then(|path| crate::settings::save_language_to(&path, language.code()));
        match result {
            Ok(_) => {
                self.language = language;
                i18n::set_current(language);
                self.settings_error = None;
                if !self.firmware.busy {
                    self.firmware.message = language.strings().firmware_idle.into();
                }
                if let Some(shell) = self.shell.as_mut() {
                    shell.language_changed();
                }
                // Explorer text is installed in the registry and Send to shortcut.
                let tx = self.tx.clone();
                std::thread::spawn(move || {
                    if let Err(error) = crate::integration::register() {
                        let message =
                            format!("{}{error:#}", i18n::current().strings().shell_menu_failed);
                        let _ = tx.send(Event::ShellError(message));
                    }
                });
            }
            Err(error) => {
                self.settings_error =
                    Some(format!("{}{error:#}", language.strings().err_language_save));
            }
        }
        cx.notify();
    }

    fn settings_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let text = self.language.strings();
        div()
            .id("settings-page")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(self.language_settings_view(cx))
            .child(self.radio_settings_view(cx))
            .child(
                div()
                    .flex_shrink_0()
                    .rounded(px(16.))
                    .bg(rgb(SURFACE))
                    .p(px(16.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().text_size(px(13.)).child(text.history_title))
                    .child(
                        button("open-transfer-history", text.history_open, false)
                            .h(px(32.))
                            .px(px(12.))
                            .on_click(cx.listener(|this, _, _, cx| this.open_history(cx))),
                    ),
            )
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
                                    .child(text.device_name),
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
                                        .child(text.change)
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
                            .child(text.device_name_hint),
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
                                        .child(self.language.name_char_count(
                                            length,
                                            crate::settings::MAX_NAME_CHARS,
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
                                                .child(text.cancel)
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
                                                .child(text.save)
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
                        text.name_saved_broadcast
                    } else {
                        text.name_saved_waiting
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
                    .child(div().text_size(px(13.)).child(text.receive_airdrop))
                    .child(div().text_size(px(11.)).text_color(rgb(MUTED)).child(
                        match &self.receiver_status {
                            Ok(()) => text.receiver_enabled.to_owned(),
                            Err(error) => error.clone(),
                        },
                    ))
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(rgb(MUTED))
                            .child(text.receiver_requires_confirm),
                    )
                    .child(div().text_size(px(11.)).child(text.save_files_to))
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(rgb(MUTED))
                            .whitespace_normal()
                            .child(
                                self.download_directory
                                    .as_ref()
                                    .map(|path| path.display().to_string())
                                    .unwrap_or_else(|| text.folder_unavailable.into()),
                            ),
                    )
                    .child(
                        button(
                            "choose-download-directory",
                            if self.picker_open {
                                text.choosing_saving
                            } else {
                                text.change_folder
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
                                .child(text.folder_change_after_receive),
                        )
                    }),
            )
            .into_any_element()
    }

    fn history_view(&self, cx: &mut Context<Self>) -> AnyElement {
        use omdrop_host::{TransferDirection, TransferFailureReason, TransferOutcome};
        let text = self.language.strings();
        let mut list = div()
            .id("history-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(8.));
        if self.history.is_empty() {
            list = list.child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(12.))
                    .text_color(rgb(MUTED))
                    .child(if self.history_loaded {
                        text.history_empty
                    } else {
                        text.history_loading
                    }),
            );
        }
        for (index, record) in self.history.iter().enumerate() {
            let direction = match record.direction {
                TransferDirection::Send => text.history_direction_send,
                TransferDirection::Receive => text.history_direction_receive,
            };
            let (outcome, color) = match record.outcome {
                TransferOutcome::Completed => (text.history_completed, 0x30d158),
                TransferOutcome::Declined => (text.history_declined, MUTED),
                TransferOutcome::Cancelled => (text.history_cancelled, MUTED),
                TransferOutcome::Failed => (text.history_failed, 0xffb56b),
            };
            let file_size_text = record
                .file_bytes
                .map(file_size)
                .unwrap_or_else(|| text.history_size_unknown.into());
            let summary = self.language.history_summary(
                record.file_count,
                &file_size_text,
                &file_size(record.transferred_bytes),
                &self.language.history_duration(record.duration_ms),
            );
            let timestamp = crate::history::format_timestamp_utc(record.finished_at_unix_ms);
            let failure = match record.failure_reason {
                Some(TransferFailureReason::Connection) => Some(text.history_failure_connection),
                Some(TransferFailureReason::Validation) => Some(text.history_failure_validation),
                Some(TransferFailureReason::Other) => Some(text.history_failure_other),
                None => None,
            };
            let destination = (record.outcome == TransferOutcome::Completed)
                .then(|| record.destination.clone())
                .flatten();
            list = list.child(
                div()
                    .id(("history-entry", index))
                    .flex_shrink_0()
                    .rounded(px(12.))
                    .bg(rgb(SURFACE))
                    .p(px(12.))
                    .flex()
                    .flex_col()
                    .gap(px(5.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(px(8.))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(12.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(format!("{direction} {}", record.peer)),
                            )
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_size(px(10.))
                                    .text_color(rgb(color))
                                    .child(outcome),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(rgb(MUTED))
                            .child(timestamp),
                    )
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(rgb(MUTED))
                            .child(summary),
                    )
                    .when_some(failure, |entry, failure| {
                        entry.child(
                            div()
                                .text_size(px(10.))
                                .text_color(rgb(0xffb56b))
                                .child(failure),
                        )
                    })
                    .when_some(destination, |entry, destination| {
                        entry.child(
                            div()
                                .id(("history-location", index))
                                .h(px(28.))
                                .px(px(10.))
                                .rounded(px(7.))
                                .bg(rgb(0x393940))
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(px(10.))
                                .cursor_pointer()
                                .child(text.history_open_location)
                                .hover(|style| style.bg(rgb(0x484850)))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.open_history_folder(destination.clone(), cx)
                                })),
                        )
                    }),
            );
        }

        let clear_controls = if self.history_clear_until.is_some() {
            div()
                .flex()
                .gap(px(8.))
                .child(
                    button("confirm-clear-history", text.history_clear_confirm, true)
                        .flex_1()
                        .on_click(cx.listener(|this, _, _, cx| this.clear_history(cx))),
                )
                .child(
                    button("cancel-clear-history", text.history_clear_cancel, false)
                        .flex_1()
                        .on_click(cx.listener(|this, _, _, cx| this.cancel_clear_history(cx))),
                )
                .into_any_element()
        } else {
            button("clear-history", text.history_clear, false)
                .w_full()
                .when(
                    self.history.is_empty() && self.history_error.is_none(),
                    |button| button.opacity(0.4).cursor(CursorStyle::Arrow),
                )
                .on_click(cx.listener(|this, _, _, cx| this.clear_history(cx)))
                .into_any_element()
        };

        div()
            .id("history-page")
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(list)
            .child(clear_controls)
            .children(
                self.history_error
                    .as_ref()
                    .map(|error| self.error_view(error)),
            )
            .into_any_element()
    }

    fn answer_incoming(&mut self, accepted: bool, cx: &mut Context<Self>) {
        if let Some(request) = self.incoming.take() {
            let active = request.active();
            let accepted = accepted && active;
            let declined_record = (!accepted && active).then(|| {
                crate::receiving::declined_history_record(
                    &request,
                    crate::history::unix_time_ms(std::time::SystemTime::now()),
                    Instant::now(),
                )
            });
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
                    self.language.strings().incoming_ended,
                    crate::receiving::NoticeKind::Info,
                );
            }
            if let Some(record) = declined_record
                && delivered
                && let Some(worker) = &self.history_worker
            {
                worker.append(record, Vec::new());
            }
            cx.notify();
        }
    }

    fn receiving_view(&self, compact: bool) -> AnyElement {
        use crate::receiving::Phase as ReceivePhase;
        let text = self.language.strings();
        let transfer = self.receive.active.as_ref().unwrap();
        let speed = self
            .receive
            .speed_bytes_per_second
            .map(|bytes| self.language.transfer_speed(&file_size(bytes)));
        let (title, detail, fraction) = match transfer.phase {
            ReceivePhase::Waiting => (
                text.receive_waiting_title,
                text.receive_waiting_detail.into(),
                None,
            ),
            ReceivePhase::Receiving {
                received,
                total: Some(total),
            } if total > 0 => {
                let fraction = (received as f64 / total as f64).clamp(0., 1.) as f32;
                (
                    text.receiving_title,
                    format!(
                        "{} / {} · {:.0}%",
                        file_size(received),
                        file_size(total),
                        fraction * 100.
                    ),
                    Some(fraction),
                )
            }
            ReceivePhase::Receiving { received, .. } => (
                text.receiving_title,
                self.language.received_size(&file_size(received)),
                None,
            ),
            ReceivePhase::Confirming => (
                text.receive_confirming_title,
                text.receive_confirming_detail.into(),
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
                    .child(
                        self.language
                            .transfer_files(&transfer.sender, transfer.files),
                    ),
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
            .when_some(speed, |view, speed| {
                view.child(div().text_size(px(11.)).text_color(rgb(MUTED)).child(speed))
            })
            .when(!compact, |view| {
                view.child(
                    div()
                        .text_size(px(10.))
                        .text_color(rgb(MUTED))
                        .whitespace_normal()
                        .child(
                            self.language
                                .saved_to(&transfer.save_dir.display().to_string()),
                        ),
                )
            })
            .into_any_element()
    }

    fn incoming_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let text = self.language.strings();
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
                    .child(text.receive_files_title),
            )
            .child(
                div().text_size(px(13.)).whitespace_normal().child(
                    self.language
                        .incoming_ask(&request.transfer.sender_name, request.transfer.files.len()),
                ),
            )
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
                    .child(
                        self.language
                            .accepted_save_to(&request.save_dir.display().to_string()),
                    ),
            )
            .child(
                button("accept-incoming", text.accept, true)
                    .on_click(cx.listener(|this, _, _, cx| this.answer_incoming(true, cx))),
            )
            .child(
                button("decline-incoming", text.decline, true)
                    .bg(rgb(SURFACE))
                    .on_click(cx.listener(|this, _, _, cx| this.answer_incoming(false, cx))),
            )
            .into_any_element()
    }

    fn empty_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let text = self.language.strings();
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
                    .child(text.share_tagline),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(px(12.))
                    .line_height(px(20.))
                    .text_color(rgb(MUTED))
                    .text_center()
                    .child(text.share_hint),
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
                text.opening_picker
            } else if self.model.validating {
                text.footer_reading
            } else {
                text.choose_files
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
        let (label, color) = kind.badge(self.language);
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
        let text = self.language.strings();
        let title = if self.model.files.len() == 1 {
            self.model.files[0]
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        } else {
            self.language.file_count(self.model.files.len())
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
                            text.connection_not_ready
                        } else if self.model.scanning {
                            text.scanning
                        } else {
                            text.no_receivers
                        },
                    )
                    .child(div().text_size(px(10.)).text_center().child(
                        if self.model.status.is_some() && !self.model.connection_ready() {
                            text.reconnect_search
                        } else {
                            text.device_awake_hint
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
                                                        text.open_in_progress
                                                    } else if self.model.files.len() > 1 {
                                                        text.open_all
                                                    } else {
                                                        text.open_file
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
                                                        text.selecting
                                                    } else if self.model.validating {
                                                        text.reading_short
                                                    } else {
                                                        text.replace_files
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
                    .child(text.send_to_nearby)
                    .child(
                        div()
                            .id("refresh")
                            .cursor_pointer()
                            .text_color(rgb(BLUE))
                            .child(if self.model.scanning {
                                if self.model.peers.is_empty() {
                                    text.searching
                                } else {
                                    text.searching_more
                                }
                            } else {
                                text.search_again
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
                        text.send
                    } else {
                        text.choose_device
                    },
                    self.model.can_send(),
                )
                .on_click(cx.listener(|this, _, _, cx| this.send(cx))),
            )
            .into_any_element()
    }

    fn transfer_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let text = self.language.strings();
        let complete = self.model.phase == Phase::Complete;
        let declined = self.model.phase == Phase::Declined;
        let subtitle = match self.model.phase {
            Phase::Preparing => text.transfer_preparing,
            Phase::Waiting => text.transfer_waiting,
            Phase::Confirming => text.transfer_confirming,
            Phase::Complete => text.transfer_complete,
            Phase::Cancelled => text.transfer_cancelled,
            Phase::Declined => text.transfer_declined,
            Phase::Cancelling => text.transfer_cancelling,
            Phase::Failed => text.transfer_failed,
            _ => text.transfer_keep_near,
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
                    .child(self.model.phase.title(self.language)),
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
            if let Some(bytes) = self.model.speed_bytes_per_second {
                view = view.child(
                    div()
                        .text_size(px(11.))
                        .text_color(rgb(MUTED))
                        .child(self.language.transfer_speed(&file_size(bytes))),
                );
            }
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
                        text.cancelling
                    } else {
                        text.cancel_send
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
                button(
                    "done",
                    if complete {
                        text.done
                    } else {
                        text.back_action
                    },
                    true,
                )
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
            .font_family(self.language.font_family())
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
                if this.language_menu_open && event.keystroke.key == "escape" {
                    this.language_menu_open = false;
                    cx.notify();
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

fn dropdown_chevron(open: bool) -> impl IntoElement {
    canvas(
        move |_, _, _| (),
        move |bounds, _, window, _| {
            let mut path = PathBuilder::stroke(px(1.25));
            let (start_y, middle_y, end_y) = if open { (6., 3., 6.) } else { (2., 5., 2.) };
            path.move_to(bounds.origin + point(px(2.), px(start_y)));
            path.line_to(bounds.origin + point(px(5.), px(middle_y)));
            path.line_to(bounds.origin + point(px(8.), px(end_y)));
            if let Ok(path) = path.build() {
                window.paint_path(path, window.text_style().color);
            }
        },
    )
    .size(px(10.))
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
