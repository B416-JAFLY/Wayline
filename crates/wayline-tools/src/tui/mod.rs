mod config;
mod language;
mod runner;

use anyhow::{bail, ensure, Context, Result};
use config::{command_text, Config};
use language::{Catalogs, InputError, Language, Notice, Status, Strings};
use ratatui::{
    backend::TestBackend,
    crossterm::{
        event::{
            self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent,
            KeyEventKind, KeyModifiers,
        },
        execute,
    },
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};
use std::{
    collections::VecDeque,
    io::{self, IsTerminal, Write},
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

const LOG_LINES: usize = 2000;
const ACCENT: Color = Color::Cyan;

#[derive(Default)]
struct Options {
    language: Language,
    preview: bool,
    help: bool,
}

impl Options {
    fn parse(args: &[String]) -> Result<Self> {
        let mut options = Self::default();
        let mut language_seen = false;
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--preview" if !options.preview && !options.help => options.preview = true,
                "--help" | "-h" if !options.help && !options.preview => options.help = true,
                "--lang" if !language_seen => {
                    language_seen = true;
                    options.language = match args.next().map(String::as_str) {
                        Some("zh-CN") => Language::Chinese,
                        Some("en-US") => Language::English,
                        _ => bail!("--lang requires zh-CN or en-US"),
                    };
                }
                _ => bail!("usage: wayline-tools tui [--lang zh-CN|en-US] [--preview | --help]"),
            }
        }
        Ok(options)
    }
}

pub fn run(args: &[String], root: &Path) -> Result<()> {
    let options = Options::parse(args)?;
    let language = options.language;
    let catalogs = Catalogs::load(&root.join(language::LOCALES_DIR))?;
    if options.preview {
        println!("{}", preview(root, language, catalogs)?);
        return Ok(());
    }
    let strings = catalogs.get(language);
    if options.help {
        println!(
            "wayline-tools tui [--lang zh-CN|en-US] [--preview | --help]\n{}",
            strings.help
        );
        return Ok(());
    }
    ensure!(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        "{}",
        strings.error.terminal_required
    );
    let root = root
        .canonicalize()
        .context(strings.error.resolve_root.clone())?;
    // Cargo metadata respects CARGO_TARGET_DIR and the user's .cargo/config.toml.
    let metadata = Command::new("cargo")
        .current_dir(&root)
        .args(["metadata", "--no-deps", "--format-version", "1", "--locked"])
        .output()
        .context(strings.error.read_target.clone())?;
    ensure!(
        metadata.status.success(),
        "{}: {}",
        strings.error.read_target_failed,
        String::from_utf8_lossy(&metadata.stderr)
    );
    let metadata: serde_json::Value = serde_json::from_slice(&metadata.stdout)?;
    let target = metadata["target_directory"]
        .as_str()
        .context(strings.error.missing_target.clone())?;
    let mut app = App::new(Config::new(target.into()), catalogs);
    app.language = language;
    let _restore = RestoreTerminal;
    let mut terminal = ratatui::try_init()
        .context(app.catalogs.get(language).error.initialize_terminal.clone())?;
    execute!(io::stdout(), EnableBracketedPaste)?;
    let result = (|| -> Result<()> {
        loop {
            app.tick();
            terminal.draw(|frame| app.render(frame))?;
            if app.quitting && app.job.is_none() {
                return Ok(());
            }
            if event::poll(Duration::from_millis(60))? {
                match event::read()? {
                    Event::Key(key) if key.kind != KeyEventKind::Release => app.key(key, &root),
                    Event::Paste(text) => app.paste(&text),
                    _ => {}
                }
            }
        }
    })();
    // Drop the worker while still owning the UI, before restoring the terminal.
    drop(app.job.take());
    result
}

struct RestoreTerminal;
impl Drop for RestoreTerminal {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), DisableBracketedPaste);
        ratatui::restore();
    }
}

struct Editor {
    field: usize,
    text: String,
    cursor: usize,
}

impl Editor {
    fn previous(&self) -> usize {
        self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map_or(0, |(i, _)| i)
    }

    fn insert(&mut self, value: &str) -> Result<(), InputError> {
        language::validate_text(value)?;
        if self.text.len() + value.len() > 4096 {
            return Err(InputError::InvalidText);
        }
        self.text.insert_str(self.cursor, value);
        self.cursor += value.len();
        Ok(())
    }

    fn key(&mut self, key: KeyEvent) -> Result<(), InputError> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('u') {
            self.text.clear();
            self.cursor = 0;
            return Ok(());
        }
        match key.code {
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.insert(&c.to_string())?
            }
            KeyCode::Left => self.cursor = self.previous(),
            KeyCode::Right => {
                self.cursor += self.text[self.cursor..]
                    .chars()
                    .next()
                    .map_or(0, char::len_utf8)
            }
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.text.len(),
            KeyCode::Backspace if self.cursor > 0 => {
                let previous = self.previous();
                self.text.drain(previous..self.cursor);
                self.cursor = previous;
            }
            KeyCode::Delete if self.cursor < self.text.len() => {
                self.text.remove(self.cursor);
            }
            _ => {}
        }
        Ok(())
    }
}

struct App {
    language: Language,
    catalogs: Catalogs,
    config: Config,
    focus: usize,
    action: ListState,
    field: ListState,
    editor: Option<Editor>,
    job: Option<runner::Job>,
    logs: VecDeque<String>,
    omitted: usize,
    scroll: u16,
    horizontal: u16,
    follow: bool,
    quitting: bool,
    status: Status,
    status_color: Color,
    note: Notice,
    started: Option<Instant>,
    elapsed: Duration,
}

impl App {
    fn new(config: Config, catalogs: Catalogs) -> Self {
        Self {
            language: Language::default(),
            catalogs,
            config,
            focus: 0,
            action: ListState::default().with_selected(Some(0)),
            field: ListState::default().with_selected(Some(0)),
            editor: None,
            job: None,
            logs: VecDeque::new(),
            omitted: 0,
            scroll: 0,
            horizontal: 0,
            follow: true,
            quitting: false,
            status: Status::Idle,
            status_color: Color::Gray,
            note: Notice::Navigation,
            started: None,
            elapsed: Duration::ZERO,
        }
    }

    fn append(&mut self, line: String) {
        if self.logs.len() == LOG_LINES {
            self.logs.pop_front();
            self.omitted += 1;
            self.scroll = self.scroll.saturating_sub(1);
        }
        self.logs.push_back(line);
    }

    fn tick(&mut self) {
        let Some(job) = self.job.as_ref() else {
            return;
        };
        self.elapsed = self.started.unwrap().elapsed();
        let lines = job.drain();
        let finished = job.finished();
        for line in lines {
            self.append(line);
        }
        if finished {
            let mut job = self.job.take().unwrap();
            let result = job.finish();
            for line in job.drain() {
                self.append(line);
            }
            match result {
                Ok(outcome) if outcome.cancelled => {
                    self.status = Status::Cancelled;
                    self.status_color = Color::Yellow;
                }
                Ok(outcome) if outcome.status.success() => {
                    self.status = Status::Success;
                    self.status_color = Color::Green;
                }
                Ok(outcome) => {
                    self.status = Status::Failed(outcome.status);
                    self.status_color = Color::Red;
                }
                Err(error) => {
                    self.status = Status::Error;
                    self.status_color = Color::Red;
                    self.append(format!("{error:#}"));
                }
            }
            self.append(format!(
                "{} · {:.1}s",
                self.status.text(self.catalogs.get(self.language)),
                self.elapsed.as_secs_f64()
            ));
        }
    }

    fn start(&mut self, root: &Path) -> Result<()> {
        if self.job.is_some() {
            self.note = Notice::Busy;
            return Ok(());
        }
        let args = self.config.args(self.action.selected().unwrap());
        let mut command = Command::new(std::env::current_exe()?);
        command.current_dir(root).args(&args);
        let job = runner::Job::spawn(command)?;
        self.logs.clear();
        self.omitted = 0;
        self.scroll = 0;
        self.horizontal = 0;
        self.follow = true;
        self.append(command_text(&args));
        self.started = Some(Instant::now());
        self.elapsed = Duration::ZERO;
        self.status = Status::Running(self.action.selected().unwrap());
        self.status_color = ACCENT;
        self.note = Notice::Running;
        self.job = Some(job);
        Ok(())
    }

    fn cancel(&mut self, quit: bool) {
        self.quitting |= quit;
        if let Some(job) = &self.job {
            job.cancel();
            self.status = Status::Cancelling(quit);
            self.status_color = Color::Yellow;
        }
    }

    fn save_logs(&mut self, root: &Path) -> Result<()> {
        let directory = root.join(".cache/tui-logs");
        std::fs::create_dir_all(&directory)?;
        let mut file = tempfile::Builder::new()
            .prefix("build-")
            .suffix(".log")
            .tempfile_in(directory)?;
        writeln!(
            file,
            "Wayline TUI {}: {}; {}: {}",
            self.catalogs.get(self.language).ui.log_snapshot,
            self.status.text(self.catalogs.get(self.language)),
            self.catalogs.get(self.language).ui.older_lines,
            self.omitted
        )?;
        for line in &self.logs {
            writeln!(file, "{line}")?;
        }
        let (_, path) = file.keep()?;
        self.note = Notice::LogSaved(path);
        Ok(())
    }

    fn paste(&mut self, text: &str) {
        if let Some(editor) = &mut self.editor {
            if let Err(error) = editor.insert(text) {
                self.note = Notice::InputError(error);
            }
        }
    }

    fn key(&mut self, key: KeyEvent, root: &Path) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.editor = None;
            self.cancel(true);
            return;
        }
        if let Some(editor) = &mut self.editor {
            match key.code {
                KeyCode::Esc => {
                    self.editor = None;
                    self.note = Notice::EditCancelled;
                }
                KeyCode::Enter => match self.config.set_text(editor.field, editor.text.clone()) {
                    Ok(()) => {
                        self.editor = None;
                        self.note = Notice::Updated;
                    }
                    Err(error) => self.note = Notice::InputError(error),
                },
                _ => {
                    if let Err(error) = editor.key(key) {
                        self.note = Notice::InputError(error);
                    }
                }
            }
            return;
        }
        let result = match key.code {
            KeyCode::Char('l' | 'L')
                if key.modifiers.difference(KeyModifiers::SHIFT).is_empty() =>
            {
                self.language.toggle();
                Ok(())
            }
            KeyCode::Char('q') => {
                self.cancel(true);
                Ok(())
            }
            KeyCode::Char('x') => {
                self.cancel(false);
                Ok(())
            }
            KeyCode::Char('s') => self.save_logs(root),
            KeyCode::Tab => {
                self.focus = (self.focus + 1) % 3;
                Ok(())
            }
            KeyCode::BackTab => {
                self.focus = (self.focus + 2) % 3;
                Ok(())
            }
            KeyCode::Up | KeyCode::Down if self.focus < 2 => {
                let (state, count) = if self.focus == 0 {
                    (&mut self.action, language::ACTION_COUNT)
                } else {
                    (&mut self.field, language::FIELD_COUNT)
                };
                let step = if key.code == KeyCode::Down {
                    1
                } else {
                    count - 1
                };
                state.select(Some((state.selected().unwrap() + step) % count));
                Ok(())
            }
            KeyCode::Enter if self.focus == 0 => self.start(root),
            KeyCode::Enter | KeyCode::Left | KeyCode::Right if self.focus == 1 => {
                if self.job.is_some() {
                    self.note = Notice::Locked;
                } else {
                    let field = self.field.selected().unwrap();
                    if field < 4 {
                        self.config.cycle(field, key.code != KeyCode::Left);
                    } else if key.code == KeyCode::Enter {
                        let text = self.config.text(field).to_owned();
                        self.editor = Some(Editor {
                            field,
                            cursor: text.len(),
                            text,
                        });
                        self.note = Notice::EditHelp;
                    }
                }
                Ok(())
            }
            KeyCode::PageUp => {
                self.scroll = self.scroll.saturating_sub(10);
                self.follow = false;
                Ok(())
            }
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_add(10);
                self.follow = false;
                Ok(())
            }
            KeyCode::End => {
                self.follow = true;
                Ok(())
            }
            KeyCode::Home if self.focus == 2 => {
                self.follow = false;
                self.scroll = 0;
                Ok(())
            }
            KeyCode::Up if self.focus == 2 => {
                self.follow = false;
                self.scroll = self.scroll.saturating_sub(1);
                Ok(())
            }
            KeyCode::Down if self.focus == 2 => {
                self.follow = false;
                self.scroll = self.scroll.saturating_add(1);
                Ok(())
            }
            KeyCode::Left if self.focus == 2 => {
                self.horizontal = self.horizontal.saturating_sub(8);
                Ok(())
            }
            KeyCode::Right if self.focus == 2 => {
                self.horizontal = self.horizontal.saturating_add(8).min(16384);
                Ok(())
            }
            _ => Ok(()),
        };
        if let Err(error) = result {
            self.note = Notice::Error(format!("{error:#}"));
        }
    }

    fn block(&self, title: String, focus: usize) -> Block<'static> {
        Block::bordered()
            .title(title)
            .border_style(Style::default().fg(if self.focus == focus {
                ACCENT
            } else {
                Color::DarkGray
            }))
    }

    fn render(&mut self, frame: &mut Frame) {
        let strings = self.catalogs.get(self.language);
        let area = frame.area();
        if area.width < 90 || area.height < 26 {
            frame.render_widget(
                Paragraph::new(format!(
                    "Wayline · {} ({}×{})\n{}\n{}",
                    strings.ui.resize,
                    area.width,
                    area.height,
                    self.status.text(strings),
                    strings.ui.small_footer
                ))
                .wrap(Wrap { trim: false }),
                area,
            );
            return;
        }
        let [header, panels, command, logs, footer] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Length(14),
            Constraint::Length(4),
            Constraint::Min(3),
            Constraint::Length(2),
        ])
        .areas(area);
        frame.render_widget(
            Paragraph::new(format!(
                "WAYLINE  /  {}     {} · {}     {} {}\n{}    {:.1}s",
                strings.ui.title,
                std::env::consts::OS,
                std::env::consts::ARCH,
                strings.ui.host,
                self.config.mode(),
                self.status.text(strings),
                self.elapsed.as_secs_f64()
            ))
            .style(
                Style::default()
                    .fg(self.status_color)
                    .add_modifier(Modifier::BOLD),
            ),
            header,
        );
        let [actions, settings] =
            Layout::horizontal([Constraint::Length(26), Constraint::Min(0)]).areas(panels);
        let items = strings
            .actions
            .iter()
            .enumerate()
            .map(|(i, value)| ListItem::new(format!("{}. {value}", i + 1)));
        let list = List::new(items)
            .block(self.block(strings.ui.tasks.clone(), 0))
            .highlight_style(Style::default().fg(Color::Black).bg(ACCENT))
            .highlight_symbol("› ");
        frame.render_stateful_widget(list, actions, &mut self.action);
        let items = strings.fields.iter().enumerate().map(|(i, label)| {
            ListItem::new(format!("{label}: {}", self.config.display(i, strings)))
        });
        let list = List::new(items)
            .block(self.block(strings.ui.settings.clone(), 1))
            .highlight_style(Style::default().fg(Color::Black).bg(ACCENT))
            .highlight_symbol("› ");
        frame.render_stateful_widget(list, settings, &mut self.field);
        let action = self.action.selected().unwrap();
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(strings.hints[action].as_str()),
                Line::from(command_text(&self.config.args(action))),
            ])
            .block(
                Block::bordered()
                    .title(strings.ui.command.as_str())
                    .border_style(Style::default().fg(Color::DarkGray)),
            )
            .wrap(Wrap { trim: false }),
            command,
        );
        let max_scroll = self
            .logs
            .len()
            .saturating_sub(logs.height.saturating_sub(2) as usize) as u16;
        self.scroll = if self.follow {
            max_scroll
        } else {
            self.scroll.min(max_scroll)
        };
        let title = format!(
            " {} · {} · {}: {} / {}: {} ",
            strings.ui.logs,
            if self.follow {
                &strings.ui.following
            } else {
                &strings.ui.browsing
            },
            strings.ui.retained,
            self.logs.len(),
            strings.ui.omitted,
            self.omitted
        );
        frame.render_widget(
            Paragraph::new(
                self.logs
                    .iter()
                    .map(|s| Line::from(s.as_str()))
                    .collect::<Vec<_>>(),
            )
            .block(self.block(title, 2))
            .scroll((self.scroll, self.horizontal)),
            logs,
        );
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(strings.ui.footer.as_str()),
                Line::styled(self.note.text(strings), Style::default().fg(Color::Yellow)),
            ]),
            footer,
        );
        if let Some(editor) = &self.editor {
            render_editor(frame, editor, strings);
        }
    }
}

fn render_editor(frame: &mut Frame, editor: &Editor, strings: &Strings) {
    let area = frame.area();
    let popup = Rect::new(area.x + 4, area.y + area.height / 2 - 2, area.width - 8, 5);
    frame.render_widget(Clear, popup);
    let block = Block::bordered()
        .title(format!(
            " {} · {} ",
            strings.fields[editor.field], strings.ui.editor
        ))
        .border_style(Style::default().fg(ACCENT));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let cursor = Line::from(&editor.text[..editor.cursor]).width() as u16;
    let scroll = cursor.saturating_sub(inner.width.saturating_sub(1));
    frame.render_widget(
        Paragraph::new(editor.text.as_str()).scroll((0, scroll)),
        inner,
    );
    frame.set_cursor_position((inner.x + cursor - scroll, inner.y));
}

fn preview(root: &Path, language: Language, catalogs: Catalogs) -> Result<String> {
    let mut app = App::new(Config::new(root.join("target")), catalogs);
    app.language = language;
    app.note = Notice::Preview;
    let mut terminal = Terminal::new(TestBackend::new(120, 38))?;
    terminal.draw(|frame| app.render(frame))?;
    Ok(buffer_text(terminal.backend().buffer()))
}

fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
    let mut result = String::new();
    for y in buffer.area.y..buffer.area.bottom() {
        let mut x = buffer.area.x;
        let mut line = String::new();
        while x < buffer.area.right() {
            let symbol = buffer[(x, y)].symbol();
            line.push_str(symbol);
            x += Line::from(symbol).width().max(1) as u16;
        }
        result.push_str(line.trim_end());
        result.push('\n');
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_reports_real_job_outcomes_and_locks_settings_during_execution() {
        for success in [true, false] {
            let mut app = App::new(Config::new("target".into()), language::test_catalogs());
            let mut command = Command::new("rustc");
            command.arg(if success {
                "--version"
            } else {
                "--wayline-invalid-test-option"
            });
            app.job = Some(runner::Job::spawn(command).unwrap());
            app.started = Some(Instant::now());
            app.status = Status::Running(1);
            app.focus = 1;
            app.field.select(Some(1));
            app.key(KeyCode::Right.into(), Path::new("."));
            assert!(!app.config.release);
            let args = app.config.args(1);
            let started = app.started;
            app.key(KeyCode::Char('l').into(), Path::new("."));
            assert_eq!(app.language, Language::English);
            assert!(app.job.is_some());
            assert_eq!(app.started, started);
            assert_eq!(app.config.args(1), args);
            assert!(app
                .status
                .text(app.catalogs.get(app.language))
                .contains(&app.catalogs.get(Language::English).actions[1]));
            let deadline = Instant::now() + Duration::from_secs(10);
            while app.job.is_some() {
                assert!(Instant::now() < deadline, "UI never observed completion");
                app.tick();
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(
                app.status_color,
                if success { Color::Green } else { Color::Red }
            );
            assert!(app.logs.len() >= 2);
            let finished_status = app.status.text(app.catalogs.get(app.language));
            app.key(KeyCode::Char('L').into(), Path::new("."));
            assert_ne!(
                app.status.text(app.catalogs.get(app.language)),
                finished_status
            );
            app.key(KeyCode::Right.into(), Path::new("."));
            assert!(app.config.release);
        }
    }

    #[test]
    fn unicode_editor_handles_cursor_delete_and_paste() {
        let mut editor = Editor {
            field: 4,
            text: "a中文".into(),
            cursor: 7,
        };
        editor.key(KeyCode::Left.into()).unwrap();
        editor.key(KeyCode::Backspace.into()).unwrap();
        assert_eq!(editor.text, "a文");
        editor.insert(" path ").unwrap();
        assert_eq!(editor.text, "a path 文");
        editor.key(KeyCode::Delete.into()).unwrap();
        assert_eq!(editor.text, "a path ");
        assert!(editor.insert("\ncommand").is_err());
    }

    #[test]
    fn navigation_and_edit_cancel_preserve_settings() {
        let mut app = App::new(Config::new("target".into()), language::test_catalogs());
        app.key(KeyCode::Tab.into(), Path::new("."));
        app.field.select(Some(6));
        app.key(KeyCode::Enter.into(), Path::new("."));
        app.paste("-edited");
        app.key(KeyCode::Esc.into(), Path::new("."));
        assert_eq!(app.config.text(6), "dist/wayline");
        app.key(KeyCode::Enter.into(), Path::new("."));
        app.paste("-edited");
        app.key(KeyCode::Enter.into(), Path::new("."));
        assert_eq!(app.config.text(6), "dist/wayline-edited");
    }

    #[test]
    fn language_hotkey_preserves_text_input_and_ignores_unrelated_keys() {
        let mut app = App::new(Config::new("target".into()), language::test_catalogs());
        app.focus = 1;
        app.field.select(Some(6));
        app.key(KeyCode::Enter.into(), Path::new("."));
        app.paste("/中文 path");
        app.key(KeyCode::Left.into(), Path::new("."));
        let editor = app.editor.as_ref().unwrap();
        let (mut text, cursor) = (editor.text.clone(), editor.cursor);
        text.insert_str(cursor, "lL");
        app.key(KeyCode::Char('l').into(), Path::new("."));
        app.key(
            KeyEvent::new(KeyCode::Char('L'), KeyModifiers::SHIFT),
            Path::new("."),
        );
        assert_eq!(app.language, Language::Chinese);
        assert_eq!(app.editor.as_ref().unwrap().text, text);
        assert_eq!(app.editor.as_ref().unwrap().cursor, cursor + 2);
        app.paste("\n");
        assert!(matches!(
            app.note,
            Notice::InputError(InputError::InvalidText)
        ));
        app.append("original 编译日志".into());
        app.key(KeyCode::Enter.into(), Path::new("."));
        assert_eq!(app.config.text(6), text);
        app.key(KeyCode::Char('l').into(), Path::new("."));
        assert_eq!(app.language, Language::English);
        app.key(
            KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL),
            Path::new("."),
        );
        app.key(
            KeyEvent::new(KeyCode::Char('l'), KeyModifiers::ALT),
            Path::new("."),
        );
        app.key(KeyCode::F(2).into(), Path::new("."));
        assert_eq!(app.language, Language::English);
        app.key(
            KeyEvent::new(KeyCode::Char('L'), KeyModifiers::SHIFT),
            Path::new("."),
        );
        assert_eq!(app.language, Language::Chinese);
        assert_eq!(app.config.text(6), text);
        assert_eq!(app.logs[0], "original 编译日志");
    }

    #[test]
    fn render_handles_resize_long_logs_and_editor_without_overflow() {
        let mut app = App::new(Config::new("target".into()), language::test_catalogs());
        for n in 0..LOG_LINES + 40 {
            app.append(format!("{n}: {}", "日志".repeat(120)));
        }
        assert_eq!(app.logs.len(), LOG_LINES);
        assert_eq!(app.omitted, 40);
        for (language, width, height) in [Language::Chinese, Language::English]
            .into_iter()
            .flat_map(|lang| [(120, 38), (90, 26), (20, 5), (1, 1)].map(|(w, h)| (lang, w, h)))
        {
            app.language = language;
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| app.render(frame)).unwrap();
            let text = buffer_text(terminal.backend().buffer());
            if width >= 90 {
                assert!(text.contains(&app.catalogs.get(language).ui.footer));
            }
            if width == 120 {
                app.editor = Some(Editor {
                    field: 4,
                    text: "中".repeat(1000),
                    cursor: 3000,
                });
                terminal.draw(|frame| app.render(frame)).unwrap();
                assert!(buffer_text(terminal.backend().buffer())
                    .contains(&app.catalogs.get(language).ui.editor));
                app.editor = None;
            }
        }
    }
}
