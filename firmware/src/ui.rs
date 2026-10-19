//! Small RGB565 UI shared by the device and host preview. No heap, web runtime or live counters.
use crate::audio::SoundStyle;
use crate::feedback::Feedback;
use crate::ui_brand;
use crate::ui_fonts::{Font, Glyph, FONT_10, FONT_12, FONT_18};
use crate::ui_orb::OrbMotion;
use crate::ui_strings::COPY;
use omdrop_proto::transfer::{Phase, Reason, Transfer, LABEL_HEIGHT, LABEL_WIDTH};

pub const WIDTH: usize = 240;
pub const HEIGHT: usize = 135;
pub const PIXELS: usize = WIDTH * HEIGHT;
const LEVELS: [u8; 4] = [40, 100, 180, 255];
const SLEEP_DELAYS: [u32; 4] = [0, 30_000, 120_000, 300_000];
const WELCOME_MS: u32 = 2400;
pub const MAIN_PAGES: u8 = 4;
const ABOUT: u8 = 4;
const HELP: u8 = 5;
const AUDIO: u8 = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Preferences {
    language: u8,
    brightness: u8,
    motion: bool,
    sleep: u8,
    sound: bool,
    volume_step: u8,
    sound_style: SoundStyle,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            language: 0,
            brightness: 2,
            motion: true,
            sleep: 0,
            sound: true,
            volume_step: 10,
            sound_style: SoundStyle::Soft,
        }
    }
}

impl Preferences {
    pub fn from_word(word: u32) -> Self {
        let language = word as u8;
        let packed = (word >> 8) as u8;
        let version = word >> 24;
        let brightness = if matches!(version, 3 | 4) {
            packed & 3
        } else {
            packed
        };
        let volume_step = if matches!(version, 3 | 4) {
            packed >> 2
        } else {
            10
        };
        let options = (word >> 16) as u8;
        let valid_options = match version {
            1 => options <= 1,
            2 => options <= 7,
            3 => options <= 15 && volume_step <= 20,
            4 => options <= 63 && volume_step <= 20,
            _ => false,
        };
        if !valid_options || language >= COPY.len() as u8 || brightness >= 4 {
            Self::default()
        } else {
            Self {
                language,
                brightness,
                motion: options & 1 != 0,
                sleep: (options >> 1) & 3,
                sound: version < 3 || options & 8 != 0,
                volume_step,
                sound_style: if version == 4 {
                    SoundStyle::from_id(options >> 4).unwrap_or_default()
                } else {
                    SoundStyle::Soft
                },
            }
        }
    }
    pub fn word(self) -> u32 {
        0x0400_0000
            | u32::from(self.language)
            | u32::from(self.brightness) << 8
            | u32::from(self.volume_step) << 10
            | u32::from(self.motion) << 16
            | u32::from(self.sleep) << 17
            | u32::from(self.sound) << 19
            | (self.sound_style as u32) << 20
    }
    pub fn brightness(self) -> u8 {
        LEVELS[usize::from(self.brightness)]
    }
    pub fn language(self) -> u8 {
        self.language
    }
    pub fn motion(self) -> bool {
        self.motion
    }
    pub fn sound(self) -> bool {
        self.sound
    }
    pub fn sound_style(self) -> SoundStyle {
        self.sound_style
    }
    pub fn volume_percent(self) -> u8 {
        self.volume_step * 5
    }
    pub fn sound_level(self) -> u8 {
        if self.sound {
            ((u16::from(self.volume_percent()) * 255 + 50) / 100) as u8
        } else {
            0
        }
    }
}

pub struct UiState {
    pub page: u8,
    preferences: Preferences,
    changed_ms: u32,
    dirty_since: Option<u32>,
    save_error: bool,
    welcome_since: Option<u32>,
    reset_since: Option<u32>,
    last_input_ms: u32,
    sleeping: bool,
    transfer_ids: [u64; 2],
    transfer_active: bool,
    preview_sound: bool,
}

impl Default for UiState {
    fn default() -> Self {
        Self::new(Preferences::default())
    }
}

impl UiState {
    pub fn new(preferences: Preferences) -> Self {
        Self {
            page: 0,
            preferences,
            changed_ms: 0,
            dirty_since: None,
            save_error: false,
            welcome_since: None,
            reset_since: None,
            last_input_ms: 0,
            sleeping: false,
            transfer_ids: [0; 2],
            transfer_active: false,
            preview_sound: false,
        }
    }
    pub fn preferences(&self) -> Preferences {
        self.preferences
    }
    pub fn take_sound_preview(&mut self) -> bool {
        core::mem::take(&mut self.preview_sound)
    }
    pub fn start_welcome(&mut self, now: u32) {
        self.welcome_since = Some(now);
        self.reset_since = None;
        self.last_input_ms = now;
        self.sleeping = false;
        self.page = 0;
    }
    pub fn sleeping(&self) -> bool {
        self.sleeping
    }
    pub fn observe_transfers(&mut self, feedback: &Feedback, now: u32) -> bool {
        let mut changed = false;
        let mut active = false;
        for (direction, lane) in feedback.lanes.iter().enumerate() {
            if let Some(value) = lane.visible(now) {
                active |= value.phase.active();
                if value.id != self.transfer_ids[direction] {
                    self.transfer_ids[direction] = value.id;
                    self.sleeping = false;
                    self.page = 0;
                    self.welcome_since = None;
                    self.reset_since = None;
                    self.last_input_ms = now;
                    self.changed_ms = now;
                    changed = true;
                }
            }
        }
        if self.transfer_active && !active {
            self.last_input_ms = now;
        }
        self.transfer_active = active;
        changed
    }
    pub fn display_brightness(&self) -> u8 {
        if self.sleeping {
            0
        } else {
            self.preferences.brightness()
        }
    }
    /// Time-based presentation updates never pause or configure the radio.
    pub fn tick(&mut self, now: u32) -> bool {
        let mut changed = false;
        if self
            .welcome_since
            .is_some_and(|start| now.wrapping_sub(start) >= WELCOME_MS)
        {
            self.welcome_since = None;
            self.changed_ms = now;
            changed = true;
        }
        if self
            .reset_since
            .is_some_and(|start| now.wrapping_sub(start) >= 10_000)
        {
            self.reset_since = None;
            changed = true;
        }
        let delay = SLEEP_DELAYS[usize::from(self.preferences.sleep)];
        if !self.sleeping
            && !self.transfer_active
            && delay != 0
            && now.wrapping_sub(self.last_input_ms) >= delay
        {
            self.sleeping = true;
            self.reset_since = None;
            changed = true;
        }
        changed
    }
    /// The public pages only control presentation. Radio configuration stays with the host.
    pub fn key(&mut self, key: u8, now: u32) -> bool {
        self.last_input_ms = now;
        if self.sleeping {
            // Consume the first key so waking cannot also change a setting or confirm a reset.
            self.sleeping = false;
            self.changed_ms = now;
            return true;
        }
        if self.reset_since.is_some() {
            self.reset_since = None;
            if key == b'\n' || key == b'\r' {
                self.preferences = Preferences::default();
                self.dirty_since = Some(now);
                self.save_error = false;
                self.changed_ms = now;
                self.page = 2;
            }
            return true;
        }
        if self.welcome_since.is_some() && key != b'l' && key != b'L' {
            self.welcome_since = None;
            self.changed_ms = now;
            return true;
        }
        let previous = self.preferences;
        let page = self.page;
        match key {
            b'\t' => {
                self.page = if self.page == AUDIO {
                    2
                } else if self.page >= MAIN_PAGES {
                    3
                } else {
                    (self.page + 1) % MAIN_PAGES
                }
            }
            b'1'..=b'4' => self.page = key - b'1',
            27 | b'\x08' => {
                self.page = if self.page == AUDIO {
                    2
                } else if self.page >= MAIN_PAGES {
                    3
                } else {
                    0
                }
            }
            b'a' | b'A' => {
                if self.page == AUDIO {
                    self.preferences.sound = !self.preferences.sound;
                } else {
                    self.page = AUDIO;
                }
            }
            b'-' | b'_' if self.page == AUDIO => {
                self.preferences.volume_step = self.preferences.volume_step.saturating_sub(1)
            }
            b'=' | b'+' if self.page == AUDIO => {
                self.preferences.volume_step = (self.preferences.volume_step + 1).min(20)
            }
            b'p' | b'P' if self.page == AUDIO => self.preview_sound = true,
            b'v' | b'V' if self.page == AUDIO => {
                self.preferences.sound_style = self.preferences.sound_style.next();
                self.preview_sound = true;
            }
            b'l' | b'L' => self.preferences.language = (self.preferences.language + 1) % 3,
            b'b' | b'B' => self.preferences.brightness = (self.preferences.brightness + 1) % 4,
            b'm' | b'M' => self.preferences.motion = !self.preferences.motion,
            b't' | b'T' => self.preferences.sleep = (self.preferences.sleep + 1) % 4,
            b's' | b'S' => self.sleeping = true,
            b'h' | b'H' => self.page = HELP,
            b'i' | b'I' => self.page = ABOUT,
            b'w' | b'W' => self.start_welcome(now),
            b'r' | b'R' if self.page >= 2 => self.reset_since = Some(now),
            _ => return false,
        }
        if previous != self.preferences {
            self.dirty_since = Some(now);
            self.save_error = false;
        }
        if page != self.page || previous.language != self.preferences.language {
            self.changed_ms = now;
        }
        true
    }
    pub fn should_save(&self, now: u32) -> bool {
        self.dirty_since
            .is_some_and(|last| now.wrapping_sub(last) >= 1200)
    }
    pub fn saved(&mut self, success: bool) {
        self.dirty_since = None;
        self.save_error = !success;
    }
}

const fn rgb(value: u32) -> u16 {
    (((value >> 19) & 31) << 11 | ((value >> 10) & 63) << 5 | (value >> 3) & 31) as u16
}
const BG: u16 = rgb(0x080c14);
const PANEL: u16 = rgb(0x121c2c);
const EDGE: u16 = rgb(0x233149);
const WHITE: u16 = rgb(0xf3f6ff);
const MUTED: u16 = rgb(0xa0aec2);
const BLUE: u16 = rgb(0x79b9ff);
const GREEN: u16 = rgb(0x7edac4);

fn blend(a: u16, b: u16, alpha: u32) -> u16 {
    let inv = 255 - alpha;
    let (a, b) = (u32::from(a), u32::from(b));
    let r = (((a >> 11) * inv + (b >> 11) * alpha + 127) / 255) << 11;
    let g = ((((a >> 5) & 63) * inv + ((b >> 5) & 63) * alpha + 127) / 255) << 5;
    let blue = ((a & 31) * inv + (b & 31) * alpha + 127) / 255;
    (r | g | blue) as u16
}

fn ease(t: u32) -> u32 {
    t * t * (765 - 2 * t) / 65025
}

pub struct Canvas {
    pub pixels: [u16; PIXELS],
    opacity: u32,
    offset_y: i32,
    orb: OrbMotion,
}

impl Default for Canvas {
    fn default() -> Self {
        Self::new()
    }
}

impl Canvas {
    pub const fn new() -> Self {
        Self {
            pixels: [0; PIXELS],
            // Keep the entire static framebuffer zero-initialized in BSS, not duplicated in flash.
            opacity: 0,
            offset_y: 0,
            orb: OrbMotion::new(),
        }
    }
    /// Sleeping panels do not render, so pause explicitly before the next wake.
    pub fn pause_motion(&mut self) {
        self.orb.pause();
    }
    fn pixel(&mut self, x: i32, y: i32, color: u16, alpha: u32) {
        let y = y + self.offset_y;
        if x < 0 || y < 0 || x >= WIDTH as i32 || y >= HEIGHT as i32 {
            return;
        }
        let p = &mut self.pixels[y as usize * WIDTH + x as usize];
        *p = blend(*p, color, alpha * self.opacity / 255);
    }
    fn rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: u16) {
        for row in y..y + h {
            for col in x..x + w {
                self.pixel(col, row, color, 255);
            }
        }
    }
    fn round_rect(&mut self, x: i32, y: i32, w: i32, h: i32, radius: i32, color: u16) {
        self.rect(x + radius, y, w - 2 * radius, h, color);
        self.rect(x, y + radius, radius, h - 2 * radius, color);
        self.rect(x + w - radius, y + radius, radius, h - 2 * radius, color);
        for dy in 0..radius {
            for dx in 0..radius {
                let mut samples = 0;
                for sy in 0..2 {
                    for sx in 0..2 {
                        let a = 2 * (radius - dx) - sx;
                        let b = 2 * (radius - dy) - sy;
                        if a * a + b * b <= 4 * radius * radius {
                            samples += 1;
                        }
                    }
                }
                let alpha = samples * 255 / 4;
                for px in [x + dx, x + w - 1 - dx] {
                    self.pixel(px, y + dy, color, alpha);
                    self.pixel(px, y + h - 1 - dy, color, alpha);
                }
            }
        }
    }
    fn ring(&mut self, cx: i32, cy: i32, radius: i32, thickness: i32, color: u16, opacity: u32) {
        let outer = radius * 2;
        let inner = (radius - thickness).max(0) * 2;
        for y in -radius..=radius {
            for x in -radius..=radius {
                let mut samples = 0;
                for sy in 0..2 {
                    for sx in 0..2 {
                        let dx = x * 2 + sx;
                        let dy = y * 2 + sy;
                        let d = dx * dx + dy * dy;
                        if d <= outer * outer && (inner == 0 || d >= inner * inner) {
                            samples += 1;
                        }
                    }
                }
                self.pixel(cx + x, cy + y, color, samples * opacity / 4);
            }
        }
    }
    fn sphere(&mut self, cx: i32, cy: i32, radius: i32) {
        let intensity = self.orb.intensity();
        for dot in self.orb.project(radius) {
            let color = blend(
                MUTED,
                blend(rgb(0x2279ca), rgb(0xb3dfff), u32::from(dot.depth)),
                intensity,
            );
            self.sphere_dot(
                cx * 256 + i32::from(dot.x_q8),
                cy * 256 + i32::from(dot.y_q8),
                i32::from(dot.radius_q8),
                color,
                u32::from(dot.alpha),
            );
        }
    }
    fn welcome_logo(&mut self, elapsed: u32, motion: bool) {
        let reveal = |delay, duration| {
            if motion {
                ease(elapsed.saturating_sub(delay).min(duration) * 255 / duration)
            } else {
                255
            }
        };
        let symbol = reveal(0, 550);
        let lettering = reveal(140, 500);
        let spread = ((255 - symbol) * 4 / 255) as i32;
        let word_rise = ((255 - lettering) * 3 / 255) as i32;
        let left = (WIDTH - ui_brand::WIDTH) as i32 / 2;
        let mut offset = 0;
        // The checked-in sprite is a compact palette/coverage stream. Decode
        // straight into the existing framebuffer with no scratch image or heap.
        for run in ui_brand::RLE.as_chunks::<2>().0 {
            let end = (offset + usize::from(run[0])).min(ui_brand::WIDTH * ui_brand::HEIGHT);
            let ink = usize::from(run[1] >> 4);
            let coverage = u32::from(run[1] & 15) * 17;
            if coverage != 0 {
                if let Some(&color) = ui_brand::COLORS.get(ink) {
                    let (dx, dy, opacity) = match ink {
                        1 => (spread, -spread, symbol),
                        2 => (-spread, spread, symbol),
                        3 => (0, word_rise, lettering),
                        _ => (0, 0, 0),
                    };
                    for pixel in offset..end {
                        self.pixel(
                            left + (pixel % ui_brand::WIDTH) as i32 + dx,
                            30 + (pixel / ui_brand::WIDTH) as i32 + dy,
                            color,
                            coverage * opacity / 255,
                        );
                    }
                }
            }
            offset = end;
        }
    }
    // Subpixel coverage keeps the small sphere smooth on the 240x135 panel.
    // Each point touches only a few pixels; no temporary image or heap is needed.
    fn sphere_dot(&mut self, cx: i32, cy: i32, radius: i32, color: u16, opacity: u32) {
        for y in (cy - radius).div_euclid(256)..=(cy + radius).div_euclid(256) {
            for x in (cx - radius).div_euclid(256)..=(cx + radius).div_euclid(256) {
                let mut covered = 0;
                for sy in [32, 96, 160, 224] {
                    for sx in [32, 96, 160, 224] {
                        let dx = x * 256 + sx - cx;
                        let dy = y * 256 + sy - cy;
                        if dx * dx + dy * dy <= radius * radius {
                            covered += 1;
                        }
                    }
                }
                if covered != 0 {
                    self.pixel(x, y, color, covered * opacity / 16);
                }
            }
        }
    }
    fn text(&mut self, x: i32, y: i32, value: &str, font: &Font, color: u16) {
        let mut x = x;
        for ch in value.chars() {
            let Some(glyph) = glyph(font, ch).or_else(|| glyph(font, '?')) else {
                continue;
            };
            for row in 0..usize::from(font.height) {
                for col in 0..usize::from(glyph.width) {
                    let index = row * usize::from(glyph.width) + col;
                    let byte = font.pixels[glyph.offset + index / 2];
                    let coverage = if index % 2 == 0 { byte >> 4 } else { byte & 15 };
                    if coverage != 0 {
                        self.pixel(
                            x + col as i32,
                            y + row as i32,
                            color,
                            u32::from(coverage) * 17,
                        );
                    }
                }
            }
            x += i32::from(glyph.advance);
        }
    }
    fn right_text(&mut self, right: i32, y: i32, value: &str, font: &Font, color: u16) {
        self.text(right - text_width(value, font), y, value, font, color);
    }
    fn center_text(&mut self, y: i32, value: &str, font: &Font, color: u16) {
        self.text(
            (WIDTH as i32 - text_width(value, font)) / 2,
            y,
            value,
            font,
            color,
        );
    }
}

fn glyph(font: &Font, ch: char) -> Option<&Glyph> {
    font.glyphs
        .binary_search_by_key(&(ch as u32), |g| g.code)
        .ok()
        .map(|i| &font.glyphs[i])
}
fn text_width(value: &str, font: &Font) -> i32 {
    value
        .chars()
        .filter_map(|c| glyph(font, c))
        .map(|g| i32::from(g.advance))
        .sum()
}

/// Link activity is not a file-transfer progress signal; no fabricated file states are shown.
pub fn render(canvas: &mut Canvas, ui: &UiState, connected: bool, now: u32) {
    render_inner(canvas, ui, connected, now, None);
}
pub fn render_with_transfers(
    canvas: &mut Canvas,
    ui: &UiState,
    connected: bool,
    now: u32,
    feedback: &Feedback,
) {
    render_inner(canvas, ui, connected, now, Some(feedback));
}
fn render_inner(
    canvas: &mut Canvas,
    ui: &UiState,
    connected: bool,
    now: u32,
    feedback: Option<&Feedback>,
) {
    canvas.pixels.fill(BG);
    canvas.opacity = 255;
    canvas.offset_y = 0;
    let copy = &COPY[usize::from(ui.preferences.language)];
    let has_transfers =
        feedback.is_some_and(|f| f.lanes.iter().any(|lane| lane.visible(now).is_some()));
    let show_orb = !ui.sleeping
        && ui.reset_since.is_none()
        && ui.welcome_since.is_none()
        && ui.page == 0
        && !has_transfers;
    // Homepage motion uses the existing host activity signal and never
    // indicates transfer progress or authorization.
    canvas
        .orb
        .update(now, connected, ui.preferences.motion, show_orb);
    if let Some(start) = ui.welcome_since {
        let elapsed = now.wrapping_sub(start);
        canvas.welcome_logo(elapsed, ui.preferences.motion);
        let reveal = if ui.preferences.motion {
            ease(elapsed.saturating_sub(300).min(400) * 255 / 400)
        } else {
            255
        };
        canvas.opacity = reveal;
        canvas.center_text(93, copy.welcome, &FONT_12, MUTED);
        canvas.opacity = 255;
        canvas.center_text(117, copy.welcome_hint, &FONT_10, MUTED);
        return;
    }
    if ui.reset_since.is_some() {
        canvas.round_rect(8, 15, 224, 99, 14, PANEL);
        canvas.center_text(27, copy.reset_title, &FONT_18, WHITE);
        canvas.center_text(60, copy.reset_detail, &FONT_12, MUTED);
        canvas.center_text(87, copy.reset_hint, &FONT_10, BLUE);
        return;
    }
    let title = match ui.page {
        1 => copy.guide,
        2 => copy.settings,
        3 => copy.tools,
        ABOUT => copy.about,
        HELP => copy.help,
        AUDIO => copy.sound_title,
        _ => copy.brand,
    };
    canvas.text(12, 7, title, &FONT_12, WHITE);
    let status = if connected { copy.online } else { copy.offline };
    let pill_width = text_width(status, &FONT_10) + 22;
    canvas.round_rect(228 - pill_width, 6, pill_width, 20, 10, PANEL);
    canvas.ring(
        237 - pill_width,
        16,
        2,
        2,
        if connected { GREEN } else { MUTED },
        255,
    );
    canvas.right_text(
        220,
        8,
        status,
        &FONT_10,
        if connected { GREEN } else { MUTED },
    );

    let elapsed = now.wrapping_sub(ui.changed_ms).min(220);
    let progress = if ui.preferences.motion {
        ease(elapsed * 255 / 220)
    } else {
        255
    };
    canvas.opacity = 96 + progress * 159 / 255;
    canvas.offset_y = ((255 - progress) * 5 / 255) as i32;
    canvas.round_rect(8, 32, 224, 77, 14, EDGE);
    canvas.round_rect(9, 33, 222, 75, 13, PANEL);
    match ui.page {
        0 if has_transfers => {
            // Transfer cards use their own geometry; do not inherit the home-card transition.
            canvas.opacity = 255;
            canvas.offset_y = 0;
            canvas.rect(0, 30, 240, 85, BG);
            transfer_cards(canvas, ui, feedback.unwrap(), now);
        }
        1 | HELP => {
            let lines = if ui.page != HELP {
                [copy.step1, copy.step2, copy.step3]
            } else if connected {
                [copy.help_phone1, copy.help_phone2, copy.help_phone3]
            } else {
                [copy.help_usb1, copy.help_usb2, copy.help_usb3]
            };
            for (i, line) in lines.iter().enumerate() {
                let y = if ui.page == 1 {
                    36 + i as i32 * 20
                } else {
                    38 + i as i32 * 23
                };
                canvas.ring(25, y + 9, 8, 8, BLUE, 32);
                canvas.text(22, y + 1, ["1", "2", "3"][i], &FONT_10, BLUE);
                canvas.text(41, y, line, &FONT_12, WHITE);
            }
            if ui.page == 1 {
                canvas.text(41, 92, copy.step3_path, &FONT_10, MUTED);
            }
        }
        3 => {
            for (i, (key, label)) in [
                ("T", copy.auto_sleep),
                ("S", copy.sleep_now),
                ("H", copy.help),
            ]
            .iter()
            .enumerate()
            {
                let y = 36 + i as i32 * 24;
                canvas.round_rect(16, y + 1, 18, 17, 5, EDGE);
                canvas.text(22, y + 2, key, &FONT_10, BLUE);
                canvas.text(42, y + 1, label, &FONT_12, MUTED);
            }
            let timeout = [
                copy.sleep_never,
                copy.sleep_30,
                copy.sleep_120,
                copy.sleep_300,
            ][usize::from(ui.preferences.sleep)];
            canvas.right_text(219, 37, timeout, &FONT_12, WHITE);
            canvas.right_text(219, 62, copy.sleep_note, &FONT_10, WHITE);
            canvas.right_text(219, 86, copy.open, &FONT_10, BLUE);
        }
        ABOUT => {
            canvas.center_text(39, "Cardputer Adv", &FONT_12, WHITE);
            canvas.center_text(
                61,
                concat!("Omdrop ", env!("CARGO_PKG_VERSION"), " / Rust"),
                &FONT_10,
                MUTED,
            );
            canvas.text(22, 84, copy.replay, &FONT_10, BLUE);
            canvas.right_text(218, 84, copy.reset, &FONT_10, BLUE);
        }
        2 => {
            for (i, (key, label)) in [
                ("L", copy.language),
                ("B", copy.brightness),
                ("M", copy.motion),
                ("A", copy.sound_title),
            ]
            .iter()
            .enumerate()
            {
                let y = 34 + i as i32 * 18;
                canvas.round_rect(16, y + 1, 18, 15, 4, EDGE);
                canvas.text(22, y + 1, key, &FONT_10, BLUE);
                canvas.text(42, y + 1, label, &FONT_12, MUTED);
            }
            canvas.right_text(219, 35, copy.language_value, &FONT_12, WHITE);
            for i in 0..4 {
                canvas.round_rect(
                    166 + i * 14,
                    60,
                    10,
                    5,
                    2,
                    if i <= i32::from(ui.preferences.brightness) {
                        BLUE
                    } else {
                        EDGE
                    },
                );
            }
            canvas.right_text(
                219,
                71,
                if ui.preferences.motion {
                    copy.motion_on
                } else {
                    copy.motion_off
                },
                &FONT_12,
                WHITE,
            );
            canvas.right_text(219, 89, copy.open, &FONT_12, BLUE);
        }
        AUDIO => {
            use core::fmt::Write;
            canvas.round_rect(16, 35, 18, 17, 5, EDGE);
            canvas.text(22, 36, "A", &FONT_10, BLUE);
            canvas.text(42, 35, copy.task_sounds, &FONT_12, MUTED);
            canvas.right_text(
                219,
                35,
                if ui.preferences.sound {
                    copy.sound_on
                } else {
                    copy.sound_off
                },
                &FONT_12,
                if ui.preferences.sound { GREEN } else { MUTED },
            );
            canvas.round_rect(16, 57, 18, 17, 5, EDGE);
            canvas.text(22, 58, "V", &FONT_10, BLUE);
            canvas.text(42, 57, copy.sound_style, &FONT_12, MUTED);
            let style = match ui.preferences.sound_style {
                SoundStyle::Soft => copy.style_soft,
                SoundStyle::EightBit => copy.style_8bit,
                SoundStyle::Chime => copy.style_chime,
                SoundStyle::Minimal => copy.style_minimal,
            };
            canvas.right_text(219, 57, style, &FONT_12, WHITE);
            canvas.text(18, 79, copy.volume, &FONT_12, MUTED);
            let mut percent = Text::new();
            let _ = write!(percent, "{}%", ui.preferences.volume_percent());
            canvas.right_text(219, 79, percent.as_str(), &FONT_12, WHITE);
            canvas.round_rect(18, 100, 202, 3, 1, EDGE);
            let width = 202 * i32::from(ui.preferences.volume_percent()) / 100;
            if width >= 4 {
                canvas.round_rect(
                    18,
                    100,
                    width,
                    3,
                    1,
                    if ui.preferences.sound { BLUE } else { MUTED },
                );
            }
        }
        _ => {
            canvas.sphere(43, 70, 25);
            canvas.text(
                80,
                43,
                if connected { copy.ready } else { copy.waiting },
                &FONT_18,
                WHITE,
            );
            canvas.text(
                80,
                72,
                if connected {
                    copy.ready_line1
                } else {
                    copy.wait_line1
                },
                &FONT_10,
                MUTED,
            );
            canvas.text(
                80,
                87,
                if connected {
                    copy.ready_line2
                } else {
                    copy.wait_line2
                },
                &FONT_10,
                MUTED,
            );
        }
    }
    canvas.opacity = 255;
    canvas.offset_y = 0;
    if ui.save_error {
        canvas.text(12, 117, copy.save_error, &FONT_10, rgb(0xf2bd83));
    } else if ui.page == AUDIO {
        canvas.text(12, 117, copy.back_hint, &FONT_10, MUTED);
        canvas.text(94, 117, copy.volume_keys, &FONT_10, MUTED);
        canvas.right_text(
            228,
            117,
            copy.sound_preview,
            &FONT_10,
            if ui.preferences.sound_level() > 0 {
                BLUE
            } else {
                MUTED
            },
        );
    } else {
        canvas.text(
            12,
            117,
            if ui.page >= MAIN_PAGES {
                copy.back_hint
            } else {
                copy.pages_hint
            },
            &FONT_10,
            MUTED,
        );
        canvas.right_text(
            228,
            117,
            if ui.page == 3 {
                copy.about_hint
            } else {
                copy.language_hint
            },
            &FONT_10,
            MUTED,
        );
        for i in 0..MAIN_PAGES {
            if i == ui.page {
                canvas.round_rect(101 + i32::from(i) * 10, 123, 7, 3, 1, BLUE);
            } else {
                canvas.ring(104 + i32::from(i) * 10, 124, 1, 1, EDGE, 255);
            }
        }
    }
}

struct Text {
    bytes: [u8; 48],
    len: usize,
}
impl Text {
    fn new() -> Self {
        Self {
            bytes: [0; 48],
            len: 0,
        }
    }
    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("")
    }
}
impl core::fmt::Write for Text {
    fn write_str(&mut self, value: &str) -> core::fmt::Result {
        if self.len + value.len() > self.bytes.len() {
            return Err(core::fmt::Error);
        }
        self.bytes[self.len..self.len + value.len()].copy_from_slice(value.as_bytes());
        self.len += value.len();
        Ok(())
    }
}
fn phase_text(value: Transfer, copy: &crate::ui_strings::Copy) -> &str {
    match value.phase {
        Phase::Idle => "",
        Phase::Preparing => copy.tf_preparing,
        Phase::Waiting if value.direction == 0 => copy.tf_wait_send,
        Phase::Waiting => copy.tf_wait_receive,
        Phase::Transferring => copy.tf_transferring,
        Phase::Confirming => copy.tf_confirming,
        Phase::Complete => copy.tf_complete,
        Phase::Cancelled => copy.tf_cancelled,
        Phase::Failed => match value.reason {
            Reason::Declined => copy.tf_declined,
            Reason::Connection => copy.tf_connection,
            Reason::Storage => copy.tf_storage,
            Reason::Interrupted => copy.tf_interrupted,
            Reason::Timeout => copy.tf_timeout,
            Reason::None => copy.tf_failed,
        },
    }
}
fn transfer_cards(canvas: &mut Canvas, ui: &UiState, feedback: &Feedback, now: u32) {
    use core::fmt::Write;
    let copy = &COPY[usize::from(ui.preferences.language)];
    let count = feedback
        .lanes
        .iter()
        .filter(|l| l.visible(now).is_some())
        .count();
    let mut row = 0;
    for lane in &feedback.lanes {
        let Some(value) = lane.visible(now) else {
            continue;
        };
        let dual = count == 2;
        let y = 32 + if dual { row * 43 } else { 0 };
        let height = if dual { 38 } else { 77 };
        let accent = match value.phase {
            Phase::Complete => GREEN,
            Phase::Failed => rgb(0xffb48a),
            Phase::Cancelled => MUTED,
            _ => BLUE,
        };
        canvas.round_rect(8, y, 224, height, 10, EDGE);
        canvas.round_rect(9, y + 1, 222, height - 2, 9, PANEL);
        canvas.text(
            18,
            y + 2,
            if value.direction == 0 {
                copy.tf_send
            } else {
                copy.tf_receive
            },
            &FONT_10,
            accent,
        );
        canvas.text(70, y + 2, phase_text(value, copy), &FONT_10, WHITE);
        let percent = if value.phase == Phase::Complete {
            Some(100)
        } else {
            value.percent()
        };
        let mut progress = Text::new();
        if let Some(p) = percent {
            let _ = write!(progress, "{p}%");
        } else if value.phase == Phase::Transferring {
            let _ = progress.write_str("...");
        }
        canvas.right_text(220, y + 2, progress.as_str(), &FONT_10, accent);
        let label_y = y + if dual { 15 } else { 23 };
        if lane.label.iter().all(|p| *p == 0) {
            canvas.text(18, label_y, copy.tf_device, &FONT_10, MUTED);
        } else {
            for dy in 0..LABEL_HEIGHT {
                for dx in 0..LABEL_WIDTH {
                    let i = dy * LABEL_WIDTH + dx;
                    let b = lane.label[i / 2];
                    let alpha = if i & 1 == 0 { b >> 4 } else { b & 15 };
                    if alpha != 0 {
                        canvas.pixel(
                            18 + dx as i32,
                            label_y + dy as i32,
                            WHITE,
                            u32::from(alpha) * 17,
                        );
                    }
                }
            }
        }
        let bar_y = y + if dual { 33 } else { 47 };
        let bar_h = if dual { 2 } else { 4 };
        canvas.round_rect(18, bar_y, 202, bar_h, 1, EDGE);
        if let Some(percent) = percent {
            let width = 202 * i32::from(percent) / 100;
            if width > 1 {
                canvas.round_rect(18, bar_y, width, bar_h, 1, accent);
            }
        } else if value.phase.active() {
            let offset = if ui.preferences.motion {
                (now % 2000) * 160 / 2000
            } else {
                80
            };
            canvas.round_rect(18 + offset as i32, bar_y, 42, bar_h, 1, accent);
        }
        if !dual {
            let mut detail = Text::new();
            if value.phase == Phase::Complete {
                let _ = detail.write_str(copy.tf_done_hint);
            } else if value.phase.active() {
                let _ = write!(detail, "{} ", copy.tf_bytes);
                if value.done < 1024 {
                    let _ = write!(detail, "{} B", value.done);
                } else if value.done < 1024 * 1024 {
                    let _ = write!(detail, "{} KB", value.done / 1024);
                } else {
                    let _ = write!(
                        detail,
                        "{}.{:01} MB",
                        value.done / 1048576,
                        value.done % 1048576 / 104858
                    );
                }
            } else {
                let _ = detail.write_str(copy.tf_error_hint);
            }
            canvas.text(18, y + 57, detail.as_str(), &FONT_10, MUTED);
        }
        row += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_logo_stream_is_bounded_and_contains_all_three_inks() {
        assert_eq!(ui_brand::RLE.len() % 2, 0);
        assert!(ui_brand::RLE.len() < 8000);
        let mut length = 0;
        let mut painted = [0usize; 4];
        for run in ui_brand::RLE.as_chunks::<2>().0 {
            assert!(run[0] > 0);
            let ink = usize::from(run[1] >> 4);
            let coverage = run[1] & 15;
            assert!(ink < ui_brand::COLORS.len());
            assert_eq!(ink == 0, coverage == 0);
            length += usize::from(run[0]);
            painted[ink] += usize::from(run[0]);
        }
        assert_eq!(length, ui_brand::WIDTH * ui_brand::HEIGHT);
        assert!(painted.iter().all(|count| *count > 100));
    }

    #[test]
    fn boot_logo_animation_stays_in_bounds_and_settles_to_the_static_logo() {
        let mut canvas = Canvas::new();
        canvas.opacity = 255;
        let mut first = None;
        for elapsed in (0..=1000).step_by(50) {
            canvas.pixels.fill(BG);
            canvas.welcome_logo(elapsed, true);
            if elapsed == 0 {
                assert!(canvas.pixels.iter().all(|pixel| *pixel == BG));
            }
            if elapsed == 200 {
                first = Some(canvas.pixels);
            }
            for (index, pixel) in canvas.pixels.iter().enumerate() {
                if *pixel != BG {
                    let (x, y) = (index % WIDTH, index / WIDTH);
                    assert!((8..232).contains(&x) && (26..86).contains(&y));
                }
            }
        }
        let settled = canvas.pixels;
        assert!(first.unwrap() != settled);
        canvas.pixels.fill(BG);
        canvas.welcome_logo(0, false);
        assert!(canvas.pixels == settled);
    }

    #[test]
    fn welcome_logo_is_consistent_across_languages_and_host_states() {
        let mut canvas = Canvas::new();
        let mut reference = None;
        for language in 0..3 {
            let mut ui = UiState::new(Preferences::from_word(0x0101_0200 | language));
            ui.start_welcome(0);
            for connected in [false, true] {
                render(&mut canvas, &ui, connected, 1200);
                let logo: Vec<_> = canvas.pixels[..90 * WIDTH].to_vec();
                if let Some(reference) = &reference {
                    assert!(*reference == logo);
                } else {
                    reference = Some(logo);
                }
            }
            ui.key(b' ', 1300); // Skip without changing a preference.
            ui.key(b'M', 1350);
            ui.start_welcome(1400);
            render(&mut canvas, &ui, false, 1400);
            let still = canvas.pixels;
            render(&mut canvas, &ui, true, 2200);
            assert!(still == canvas.pixels);
        }
    }

    #[test]
    fn point_spheres_stay_inside_their_reserved_illustration_areas() {
        let mut canvas = Canvas::new();
        for (cx, cy, radius, left, top, right, bottom) in
            [(43, 70, 25, 12, 39, 75, 102), (120, 35, 21, 94, 9, 147, 62)]
        {
            for frame in 0..100 {
                canvas.pixels.fill(BG);
                canvas.opacity = 255;
                canvas.orb.update(frame * 200, true, true, true);
                canvas.sphere(cx, cy, radius);
                let mut painted = 0;
                for (index, pixel) in canvas.pixels.iter().enumerate() {
                    if *pixel != BG {
                        let x = (index % WIDTH) as i32;
                        let y = (index / WIDTH) as i32;
                        assert!((left..right).contains(&x), "x={x}");
                        assert!((top..bottom).contains(&y), "y={y}");
                        painted += 1;
                    }
                }
                assert!(painted > 100);
            }
        }
    }

    #[test]
    fn homepage_motion_leaves_text_fixed_and_disconnect_settles() {
        let mut canvas = Canvas::new();
        let ui = UiState::default();
        for now in (0..=1000).step_by(50) {
            render(&mut canvas, &ui, true, now);
        }
        let before = canvas.pixels;
        for now in (1050..=2000).step_by(50) {
            render(&mut canvas, &ui, true, now);
            for (index, (&old, &new)) in before.iter().zip(&canvas.pixels).enumerate() {
                let (x, y) = (index % WIDTH, index / WIDTH);
                if !(12..75).contains(&x) || !(39..102).contains(&y) {
                    assert_eq!(old, new, "animation moved UI at {x},{y}");
                }
            }
        }
        assert_ne!(before, canvas.pixels);
        for now in (2050..=3000).step_by(50) {
            render(&mut canvas, &ui, false, now);
        }
        let still = canvas.pixels;
        for now in (3050..=4000).step_by(50) {
            render(&mut canvas, &ui, false, now);
            assert_eq!(still, canvas.pixels);
        }
    }

    #[test]
    fn styles_cycle_preview_and_persist_without_changing_mute_or_volume() {
        let mut ui = UiState::default();
        ui.key(b'V', 0); // Not a global shortcut.
        assert_eq!(ui.preferences.sound_style(), SoundStyle::Soft);
        ui.key(b'3', 1);
        ui.key(b'A', 2);
        ui.key(b'A', 3); // Muted selection must remain muted.
        ui.key(b'-', 4);
        for (i, style) in [
            SoundStyle::EightBit,
            SoundStyle::Chime,
            SoundStyle::Minimal,
            SoundStyle::Soft,
        ]
        .iter()
        .enumerate()
        {
            ui.key(b'V', 10 + i as u32);
            assert_eq!(ui.preferences.sound_style(), *style);
            assert_eq!(ui.preferences.volume_percent(), 45);
            assert_eq!(ui.preferences.sound_level(), 0);
            assert!(ui.take_sound_preview());
            assert!(!ui.take_sound_preview());
            assert_eq!(
                Preferences::from_word(ui.preferences.word()),
                ui.preferences
            );
        }
        assert!(!ui.should_save(1212));
        assert!(ui.should_save(1213));
    }

    #[test]
    fn v3_migration_preserves_all_settings_and_v4_rejects_reserved_bits() {
        for language in 0..3 {
            for brightness in 0..4 {
                for options in 0..16 {
                    for volume in 0..=20 {
                        let raw =
                            0x0300_0000 | language | brightness << 8 | volume << 10 | options << 16;
                        let migrated = Preferences::from_word(raw);
                        assert_eq!(migrated.sound_style(), SoundStyle::Soft);
                        assert_eq!(migrated.word(), (raw & 0x00ff_ffff) | 0x0400_0000);
                        assert_eq!(Preferences::from_word(migrated.word()), migrated);
                    }
                }
            }
        }
        for raw in [0x0449_2800, 0x0489_2800, 0x0409_5400, 0x0409_2803] {
            assert_eq!(Preferences::from_word(raw), Preferences::default());
        }
    }
    #[test]
    fn volume_moves_in_five_percent_steps_clamps_and_mute_keeps_the_level() {
        let mut ui = UiState::default();
        assert!(ui.preferences.sound());
        assert_eq!(ui.preferences.volume_percent(), 50);
        ui.key(b'3', 0);
        ui.key(b'A', 1);
        assert_eq!(ui.page, AUDIO);
        ui.key(b'-', 2);
        assert_eq!(ui.preferences.volume_percent(), 45);
        ui.key(b'=', 3); // The physical +/= key does not require Shift.
        assert_eq!(ui.preferences.volume_percent(), 50);
        for i in 0..30 {
            ui.key(b'-', 10 + i);
        }
        assert_eq!(ui.preferences.volume_percent(), 0);
        assert_eq!(ui.preferences.sound_level(), 0);
        for step in 1..=20 {
            ui.key(b'+', 100 + step);
            assert_eq!(u32::from(ui.preferences.volume_percent()), step * 5);
            assert_eq!(
                Preferences::from_word(ui.preferences.word()),
                ui.preferences
            );
        }
        ui.key(b'+', 130);
        assert_eq!(ui.preferences.volume_percent(), 100);
        assert_eq!(ui.preferences.sound_level(), 255);
        ui.key(b'A', 140);
        assert!(!ui.preferences.sound());
        assert_eq!(ui.preferences.sound_level(), 0);
        ui.key(b'-', 150);
        assert_eq!(ui.preferences.volume_percent(), 95);
        assert_eq!(ui.preferences.sound_level(), 0);
        let saved = Preferences::from_word(ui.preferences.word());
        assert!(!saved.sound());
        assert_eq!(saved.volume_percent(), 95);
        ui.key(b'A', 160);
        assert!(ui.preferences.sound_level() > 0);
        ui.key(b'P', 170);
        assert!(ui.take_sound_preview());
        assert!(!ui.take_sound_preview());
        ui.key(b'\x08', 180);
        assert_eq!(ui.page, 2);
        assert!(!ui.should_save(1359));
        assert!(ui.should_save(1360));
        assert_eq!(Preferences::from_word(0x0300_5400), Preferences::default()); // 105% is invalid.
        assert_eq!(Preferences::from_word(0x0310_0000), Preferences::default());
        // Unknown option.
    }
    #[test]
    fn transfers_wake_but_respect_manual_sleep_and_do_not_auto_sleep() {
        use omdrop_proto::transfer::{FRAME_LEN, HEADER_LEN};
        let mut ui = UiState::default();
        ui.key(b'T', 0); // 30 seconds.
        ui.key(b'S', 1);
        let mut feedback = Feedback::new();
        let mut packet = [0; FRAME_LEN];
        let value = Transfer {
            id: 1,
            phase: Phase::Transferring,
            ..Transfer::idle(0)
        };
        packet[..HEADER_LEN].copy_from_slice(&value.encode());
        assert!(feedback.apply(&packet, 100));
        ui.observe_transfers(&feedback, 100);
        assert!(!ui.sleeping);
        assert!(feedback.apply(&packet, 40_000));
        ui.observe_transfers(&feedback, 40_000);
        ui.tick(40_000);
        assert!(!ui.sleeping);
        ui.key(b'S', 40_001);
        ui.observe_transfers(&feedback, 40_002);
        assert!(ui.sleeping);
        ui.key(b'1', 40_003);
        ui.observe_transfers(&feedback, 48_000);
        ui.tick(48_000);
        assert!(!ui.sleeping);
        ui.tick(78_000);
        assert!(ui.sleeping);
    }
    #[test]
    fn invalid_preferences_recover_and_changes_debounce_across_timer_wrap() {
        for raw in [0, u32::MAX, 0x0101_0203, 0x0101_0400, 0x0102_0200] {
            assert_eq!(Preferences::from_word(raw), Preferences::default());
        }
        let mut ui = UiState::default();
        assert!(!ui.key(b'f', 0));
        assert!(!ui.key(b']', 0));
        assert!(ui.key(b'L', u32::MAX - 100));
        assert!(!ui.should_save(500));
        assert!(ui.should_save(1200));
        let saved = ui.preferences().word();
        assert_eq!(Preferences::from_word(saved).language(), 1);
        ui.saved(true);
        assert!(!ui.should_save(2000));
        ui.key(b'M', 3000);
        assert!(!ui.preferences.motion());
        ui.key(b'B', 3001);
        assert_eq!(ui.preferences.brightness(), 255);
    }
    #[test]
    fn translated_text_fits_and_required_glyphs_exist() {
        for copy in &COPY {
            for style in [
                copy.style_soft,
                copy.style_8bit,
                copy.style_chime,
                copy.style_minimal,
            ] {
                assert!(text_width(copy.sound_style, &FONT_12) + text_width(style, &FONT_12) < 165);
            }
            assert!(12 + text_width(copy.back_hint, &FONT_10) + 8 <= 94);
            assert!(
                94 + text_width(copy.volume_keys, &FONT_10) + 8
                    <= 228 - text_width(copy.sound_preview, &FONT_10)
            );
            assert!(
                text_width(copy.task_sounds, &FONT_12) + text_width(copy.sound_off, &FONT_12) < 165
            );
            assert!(
                text_width(copy.volume_keys, &FONT_10) + text_width(copy.sound_preview, &FONT_10)
                    < 192
            );
            assert!(
                12 + text_width(copy.sound_title, &FONT_12) + 8
                    < 228 - text_width(copy.online, &FONT_10) - 22
            );
            for text in [
                copy.tf_preparing,
                copy.tf_wait_send,
                copy.tf_wait_receive,
                copy.tf_transferring,
                copy.tf_confirming,
                copy.tf_complete,
                copy.tf_cancelled,
                copy.tf_declined,
                copy.tf_connection,
                copy.tf_storage,
                copy.tf_interrupted,
                copy.tf_timeout,
                copy.tf_failed,
            ] {
                assert!(
                    text_width(text, &FONT_10) <= 117,
                    "transfer phase too wide: {text}"
                );
            }
            for text in [copy.tf_send, copy.tf_receive] {
                assert!(
                    text_width(text, &FONT_10) <= 46,
                    "direction too wide: {text}"
                );
            }
            for text in [copy.ready, copy.waiting] {
                assert!(text_width(text, &FONT_18) <= 144, "{text}");
            }
            for text in [
                copy.ready_line1,
                copy.ready_line2,
                copy.wait_line1,
                copy.wait_line2,
            ] {
                assert!(text_width(text, &FONT_10) <= 144, "{text}");
            }
            for text in [
                copy.step1,
                copy.step2,
                copy.step3,
                copy.help_usb1,
                copy.help_usb2,
                copy.help_usb3,
                copy.help_phone1,
                copy.help_phone2,
                copy.help_phone3,
            ] {
                assert!(text_width(text, &FONT_12) <= 182, "{text}");
            }
            assert!(
                text_width(copy.step3_path, &FONT_10) <= 182,
                "{}",
                copy.step3_path
            );
            for ch in copy.step3_path.chars() {
                assert!(
                    glyph(&FONT_10, ch).is_some(),
                    "missing guide path glyph: {ch}"
                );
            }
            for text in [copy.guide, copy.settings, copy.tools, copy.about, copy.help] {
                assert!(
                    12 + text_width(text, &FONT_12) + 8
                        < 228 - text_width(copy.online, &FONT_10) - 22,
                    "{text}"
                );
            }
            assert!(
                text_width(copy.language, &FONT_12) + text_width(copy.language_value, &FONT_12)
                    < 169
            );
            for timeout in [
                copy.sleep_never,
                copy.sleep_30,
                copy.sleep_120,
                copy.sleep_300,
            ] {
                assert!(
                    text_width(copy.auto_sleep, &FONT_12) + text_width(timeout, &FONT_12) < 169
                );
            }
            assert!(
                text_width(copy.sleep_now, &FONT_12) + text_width(copy.sleep_note, &FONT_10) < 169
            );
            assert!(text_width(copy.reset_title, &FONT_18) <= 204);
            for text in [copy.welcome, copy.reset_detail] {
                assert!(text_width(text, &FONT_12) <= 212, "{text}");
            }
            for text in [copy.reset_hint, copy.welcome_hint] {
                assert!(text_width(text, &FONT_10) <= 216, "{text}");
            }
            assert!(text_width(copy.replay, &FONT_10) + text_width(copy.reset, &FONT_10) < 188);
        }
        for font in [&FONT_10, &FONT_12, &FONT_18] {
            for glyph in font.glyphs {
                assert!(
                    glyph.offset
                        + (usize::from(glyph.width) * usize::from(font.height)).div_ceil(2)
                        <= font.pixels.len()
                );
            }
        }
    }
    #[test]
    fn reduced_motion_freezes_animation_and_every_language_page_renders() {
        let mut canvas = Canvas::new();
        let mut ui = UiState::default();
        ui.key(b'M', 0);
        render(&mut canvas, &ui, true, 1000);
        let still = canvas.pixels;
        render(&mut canvas, &ui, true, 1800);
        assert_eq!(still, canvas.pixels);
        ui.key(b'M', 1800);
        render(&mut canvas, &ui, true, 1900);
        assert_eq!(still, canvas.pixels); // Resume from the same pose without catching up.
        render(&mut canvas, &ui, true, 1950);
        assert_ne!(still, canvas.pixels);
        for _ in 0..3 {
            ui.key(b'L', 0);
            for page in *b"1234HIA" {
                ui.key(page, 0);
                render(&mut canvas, &ui, true, 1000);
                assert!(canvas.pixels.iter().filter(|&&p| p != BG).count() > 10000);
            }
        }
    }

    #[test]
    fn old_preferences_migrate_without_changing_existing_settings() {
        for language in 0..3 {
            for brightness in 0..4 {
                for motion in 0..2 {
                    let old = Preferences::from_word(
                        0x0100_0000 | language | brightness << 8 | motion << 16,
                    );
                    assert_eq!(old.language, language as u8);
                    assert_eq!(old.brightness, brightness as u8);
                    assert_eq!(old.motion, motion != 0);
                    assert_eq!(old.sleep, 0);
                    assert_eq!(Preferences::from_word(old.word()), old);
                }
            }
        }
        for options in 0..8 {
            let prefs = Preferences::from_word(0x0200_0000 | options << 16);
            assert_eq!(prefs.sleep, (options >> 1) as u8);
            assert!(prefs.sound());
            assert_eq!(prefs.volume_percent(), 50);
            assert_eq!(Preferences::from_word(prefs.word()), prefs);
        }
        assert_eq!(Preferences::from_word(0x0208_0000), Preferences::default());
    }

    #[test]
    fn welcome_is_skippable_localized_and_wrap_safe() {
        let mut ui = UiState::default();
        let start = u32::MAX - 500;
        ui.start_welcome(start);
        ui.key(b'L', start + 1);
        assert_eq!(ui.preferences.language, 1);
        assert!(ui.welcome_since.is_some());
        assert!(!ui.tick(start.wrapping_add(WELCOME_MS - 1)));
        assert!(ui.tick(start.wrapping_add(WELCOME_MS)));
        assert!(ui.welcome_since.is_none());
        ui.key(b'W', 4000);
        let preferences = ui.preferences;
        ui.key(b'B', 4010);
        assert!(ui.welcome_since.is_none());
        assert_eq!(ui.preferences, preferences); // A skip is not also a brightness action.
    }

    #[test]
    fn sleep_preserves_preferences_and_consumes_the_waking_key() {
        let mut ui = UiState::default();
        let start = u32::MAX - 50;
        ui.key(b'T', start);
        assert!(!ui.tick(start.wrapping_add(29_999)));
        assert!(ui.tick(start.wrapping_add(30_000)));
        assert!(ui.sleeping());
        assert_eq!(ui.display_brightness(), 0);
        assert!(ui.should_save(start.wrapping_add(30_000)));
        let prefs = ui.preferences;
        ui.key(b'L', 40_000);
        assert!(!ui.sleeping());
        assert_eq!(ui.preferences, prefs);
        assert_eq!(ui.display_brightness(), prefs.brightness());
        ui.key(b'S', 40_001);
        assert!(ui.sleeping());
        ui.key(b'R', 40_002);
        assert!(ui.reset_since.is_none());
        assert!(!ui.sleeping());
    }

    #[test]
    fn reset_requires_confirmation_and_expires_without_changing_preferences() {
        let mut ui = UiState::default();
        ui.key(b'L', 0);
        ui.key(b'I', 1);
        ui.key(b'R', 2);
        assert!(ui.reset_since.is_some());
        ui.key(b'\x08', 3);
        assert_eq!(ui.preferences.language, 1);
        assert!(ui.reset_since.is_none());
        ui.key(b'R', 10);
        ui.tick(10_010);
        ui.key(b'\n', 10_011);
        assert_eq!(ui.preferences.language, 1);
        ui.key(b'R', 11_000);
        ui.key(b'\n', 11_001);
        assert_eq!(ui.preferences, Preferences::default());
        assert_eq!(ui.page, 2);
        assert!(ui.should_save(12_201));
    }
}
