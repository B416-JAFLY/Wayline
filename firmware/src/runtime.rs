//! ESP-IDF FFI is confined here. Driver calls never run under the state spinlock.

#[cfg(feature = "cardputer-ui")]
use crate::audio::{waveform, Cue, Player, TaskSounds};
#[cfg(feature = "cardputer-ui")]
use crate::ui::{self, Canvas, Preferences, UiState};
use crate::{
    aggregation::{self, Receiver},
    protocol::{self, Command, Encoder, FrameDecoder},
    state::{self, PacketKind, RadioState},
    uplink::{Batch, BATCH_FRAMES},
};
use core::{
    cell::UnsafeCell,
    ffi::{c_char, c_void, CStr},
    slice,
};
use wayline_proto::{FilterMode, RadioMessage, PROTO_MAX_FRAME};

use core::sync::atomic::{AtomicU32, Ordering};
static BA_GENERATION: AtomicU32 = AtomicU32::new(0);
static BA_RX: AtomicU32 = AtomicU32::new(0);
static BA_RX_DROPS: AtomicU32 = AtomicU32::new(0);
static BA_CONTROL_DROPS: AtomicU32 = AtomicU32::new(0);
static TX_REPORT_DROPS: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "cardputer-ui")]
static UI_STAGE: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "cardputer-ui")]
static UI_TICKS: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "cardputer-ui")]
static UI_FRAMES: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "cardputer-ui")]
static UI_KEYS: AtomicU32 = AtomicU32::new(0);

extern "C" {
    fn owl_hal_init();
    fn owl_hal_radio_init(mac: *mut u8);
    fn owl_hal_default_channel() -> u8;
    fn owl_hal_apply(channel: u8, filter: u8) -> i32;
    fn owl_hal_set_channel(channel: u8) -> i32;
    fn owl_hal_set_mac(mac: *const u8, actual: *mut u8, channel: u8, filter: u8) -> i32;
    fn owl_hal_transmit(rate: u8, frame: *const u8, len: usize) -> i32;
    fn owl_hal_config_lock();
    fn owl_hal_config_try_lock() -> bool;
    fn owl_hal_config_unlock();
    fn owl_hal_state_lock();
    fn owl_hal_state_unlock();
    fn owl_hal_tx_lock();
    fn owl_hal_tx_unlock();
    fn owl_hal_read(out: *mut u8, cap: usize) -> usize;
    fn owl_hal_write(data: *const u8, len: usize) -> usize;
    fn owl_hal_enqueue(
        kind: u8,
        header: *const u8,
        header_len: usize,
        frame: *const u8,
        len: usize,
    ) -> bool;
    fn owl_hal_dequeue(len: *mut usize, wait: bool) -> *const u8;
    fn owl_hal_release(item: *const u8);
    fn owl_hal_time_us() -> u64;
    fn owl_hal_delay_ms(ms: u32);
    fn owl_hal_target() -> *const c_char;
    fn owl_hal_idf_version() -> *const c_char;
    fn owl_hal_start_tasks();
    fn owl_hal_heap_free() -> u32;
    fn owl_hal_heap_minimum() -> u32;
    fn owl_hal_link_core() -> u32;
    fn owl_hal_ba_enabled() -> bool;
    fn owl_hal_ba_enqueue(frame: *const u8, len: usize, generation: u32) -> bool;
    fn owl_hal_ba_dequeue(frame: *mut u8, cap: usize, generation: *mut u32) -> usize;
    fn owl_hal_ba_clear_queue();
    fn owl_hal_ba_start(peer: *const u8, tid: u8, ssn: u16, window: u8) -> bool;
    fn owl_hal_ba_stop();
    #[cfg(feature = "cardputer-ui")]
    fn radio_ui_start();
    #[cfg(feature = "cardputer-ui")]
    fn radio_ui_poll_key(key: *mut u8, pressed: *mut bool) -> bool;
    #[cfg(feature = "cardputer-ui")]
    fn radio_ui_set_brightness(brightness: u8);
    #[cfg(feature = "cardputer-ui")]
    fn radio_ui_present(pixels: *const u16);
    #[cfg(feature = "cardputer-ui")]
    fn radio_ui_load_preferences() -> u32;
    #[cfg(feature = "cardputer-ui")]
    fn radio_ui_save_preferences(preferences: u32) -> bool;
    #[cfg(feature = "cardputer-ui")]
    fn radio_ui_set_volume(volume: u8);
    #[cfg(feature = "cardputer-ui")]
    fn radio_ui_tone(frequency: u16, duration_ms: u16, samples: *const u8, len: usize) -> bool;
    #[cfg(feature = "cardputer-ui")]
    fn radio_ui_stop_sound();
    #[cfg(feature = "cardputer-ui")]
    fn radio_ui_debug_state() -> u32;
}

// Each cell has an exclusive owner: the corresponding lock, or the single uplink task.
struct Shared<T>(UnsafeCell<T>);
// SAFETY: access follows the per-instance ownership contract documented at each static.
unsafe impl<T: Send> Sync for Shared<T> {}
static STATE: Shared<RadioState> = Shared(UnsafeCell::new(RadioState::new()));
static ENCODER: Shared<Encoder> = Shared(UnsafeCell::new(Encoder::new()));
// The configuration mutex owns this state. No hardware calls hold STATE's spinlock.
static AGGREGATION: Shared<Receiver> = Shared(UnsafeCell::new(Receiver::new()));

struct Guard(unsafe extern "C" fn());
impl Drop for Guard {
    fn drop(&mut self) {
        unsafe { (self.0)() }
    }
}

fn with_state<R>(f: impl FnOnce(&mut RadioState) -> R) -> R {
    // SAFETY: the callback is private to this module and never reenters this accessor,
    // blocks, or retains a reference. The SMP critical section excludes every other accessor.
    unsafe {
        owl_hal_state_lock();
        let _guard = Guard(owl_hal_state_unlock);
        f(&mut *STATE.0.get())
    }
}

struct ConfigGuard;
impl Drop for ConfigGuard {
    fn drop(&mut self) {
        unsafe { owl_hal_config_unlock() };
    }
}

fn config_guard() -> ConfigGuard {
    unsafe { owl_hal_config_lock() };
    ConfigGuard
}

fn with_aggregation<R>(_guard: &ConfigGuard, f: impl FnOnce(&mut Receiver) -> R) -> R {
    // SAFETY: only the configuration-mutex owner can construct the guard. No
    // caller retains references or reenters; callbacks use a nonblocking try-lock.
    unsafe { f(&mut *AGGREGATION.0.get()) }
}

struct BaHardware;
impl aggregation::Hardware for BaHardware {
    fn start(&mut self, peer: &[u8; 6], tid: u8, ssn: u16, window: u8) -> bool {
        // SAFETY: HAL copies all six peer bytes synchronously; callers own config.
        unsafe { owl_hal_ba_start(peer.as_ptr(), tid, ssn, window) }
    }
    fn stop(&mut self) {
        unsafe { owl_hal_ba_stop() };
    }
    fn transmit(&mut self, frame: &[u8]) -> bool {
        // SAFETY: the driver copies the frame before returning. These are bounded
        // management frames; the hardware itself generates the immediate BA.
        let error = unsafe { owl_hal_transmit(24, frame.as_ptr(), frame.len()) };
        with_state(|s| {
            increment(if error == 0 {
                &mut s.stats.tx_ok
            } else {
                &mut s.stats.tx_err
            })
        });
        error == 0
    }
}

fn reset_aggregation(guard: &ConfigGuard) {
    BA_GENERATION.fetch_add(1, Ordering::Relaxed);
    unsafe { owl_hal_ba_clear_queue() };
    with_aggregation(guard, |rx| rx.reset(&mut BaHardware));
}

fn send(kind: RadioMessage, a: &[u8], b: &[u8]) {
    // SAFETY: the TX mutex owns the encoder for the entire synchronous driver write.
    unsafe {
        owl_hal_tx_lock();
        let _guard = Guard(owl_hal_tx_unlock);
        if let Ok(bytes) = (&mut *ENCODER.0.get()).encode(kind as u8, a, b) {
            owl_hal_write(bytes.as_ptr(), bytes.len());
        }
    }
}

fn now_ms() -> u32 {
    (unsafe { owl_hal_time_us() } / 1000) as u32
}
fn increment(value: &mut u32) {
    *value = value.wrapping_add(1);
}

fn set_channel(channel: u8) -> i32 {
    let guard = config_guard();
    if with_state(|s| s.channel != channel) {
        reset_aggregation(&guard);
    }
    let error = unsafe { owl_hal_set_channel(channel) };
    if error == 0 {
        with_state(|s| s.channel = channel);
    }
    error
}

fn set_filter(filter: FilterMode) -> i32 {
    let guard = config_guard();
    if filter != FilterMode::Awdl {
        reset_aggregation(&guard);
    }
    let channel = with_state(|s| s.channel);
    let error = unsafe { owl_hal_apply(channel, filter as u8) };
    if error == 0 {
        with_state(|s| s.filter = filter);
    }
    error
}

fn dispatch(message: &[u8]) {
    let request = message[0]; // FrameDecoder returns at least a type byte.
    let now = now_ms();
    with_state(|s| s.record_command(request, now));
    let error = match protocol::parse_command(message) {
        Err(error) => error,
        Ok(Command::Hello) => {
            let mac = with_state(|s| s.mac);
            // SAFETY: the HAL returns static, NUL-terminated IDF string literals.
            let body = unsafe {
                protocol::hello_body(
                    &mac,
                    CStr::from_ptr(owl_hal_target()).to_bytes(),
                    CStr::from_ptr(owl_hal_idf_version()).to_bytes(),
                )
            };
            send(RadioMessage::HelloResponse, &body, &[]);
            return;
        }
        Ok(Command::Time) => {
            send(
                RadioMessage::TimeResponse,
                &unsafe { owl_hal_time_us() }.to_le_bytes(),
                &[],
            );
            return;
        }
        Ok(Command::Stats) => {
            let stats = with_state(|s| s.stats);
            send(RadioMessage::StatsResponse, &stats.wire_bytes(), &[]);
            return;
        }
        Ok(Command::UplinkStatus) => {
            let stats = with_state(|s| s.uplink);
            let body = unsafe {
                stats.wire_bytes(
                    owl_hal_heap_free(),
                    owl_hal_heap_minimum(),
                    owl_hal_link_core(),
                )
            };
            send(RadioMessage::StatsResponse, &body, &[]);
            return;
        }
        Ok(Command::AggregationStatus) => {
            let values = {
                let guard = config_guard();
                with_aggregation(&guard, |rx| {
                    let active = rx.active();
                    let c = rx.counters;
                    [
                        1,
                        u32::from(unsafe { owl_hal_ba_enabled() }),
                        u32::from(active.is_some()),
                        active.map_or(255, |s| u32::from(s.0)),
                        active.map_or(0, |s| u32::from(s.1)),
                        c.requests,
                        c.accepted,
                        c.declined,
                        c.response_errors,
                        c.ended,
                        BA_RX.load(Ordering::Relaxed),
                        BA_RX_DROPS.load(Ordering::Relaxed),
                        BA_CONTROL_DROPS.load(Ordering::Relaxed),
                        TX_REPORT_DROPS.load(Ordering::Relaxed),
                    ]
                })
            };
            let mut body = [0u8; 56];
            for (slot, value) in body.chunks_exact_mut(4).zip(values) {
                slot.copy_from_slice(&value.to_le_bytes());
            }
            send(RadioMessage::StatsResponse, &body, &[]);
            return;
        }
        Ok(Command::UiStatus) => {
            #[cfg(feature = "cardputer-ui")]
            {
                let values = [
                    now,
                    UI_STAGE.load(Ordering::Relaxed),
                    UI_TICKS.load(Ordering::Relaxed),
                    UI_FRAMES.load(Ordering::Relaxed),
                    UI_KEYS.load(Ordering::Relaxed),
                    unsafe { radio_ui_debug_state() },
                ];
                let mut body = [0u8; 24];
                for (slot, value) in body.chunks_exact_mut(4).zip(values) {
                    slot.copy_from_slice(&value.to_le_bytes());
                }
                send(RadioMessage::StatsResponse, &body, &[]);
                return;
            }
            #[cfg(not(feature = "cardputer-ui"))]
            {
                protocol::NOT_SUPPORTED
            }
        }
        Ok(Command::SetChannel(channel)) => set_channel(channel),
        Ok(Command::Transfer(bytes)) => {
            if with_state(|s| s.feedback.apply(bytes, now)) {
                0
            } else {
                protocol::INVALID_ARG
            }
        }
        Ok(Command::TransferStatus(direction)) => {
            let body = with_state(|s| s.feedback.header(direction, now));
            send(RadioMessage::TransferResponse, &body, &[]);
            return;
        }
        Ok(Command::SetFilter(filter)) => set_filter(filter),
        Ok(Command::SetMac(mac)) => {
            let guard = config_guard();
            reset_aggregation(&guard);
            let (mut actual, channel, filter) = with_state(|s| (s.mac, s.channel, s.filter));
            let result = unsafe {
                owl_hal_set_mac(mac.as_ptr(), actual.as_mut_ptr(), channel, filter as u8)
            };
            with_state(|s| s.mac = actual);
            result
        }
        Ok(Command::Tx(tx)) => {
            let _guard = config_guard();
            let mut error = 0;
            for i in 0..tx.count {
                error = unsafe { owl_hal_transmit(tx.rate, tx.frame.as_ptr(), tx.frame.len()) };
                with_state(|s| {
                    increment(if error == 0 {
                        &mut s.stats.tx_ok
                    } else {
                        &mut s.stats.tx_err
                    })
                });
                if error != 0 {
                    break;
                }
                if tx.gap_ms != 0 && i + 1 < tx.count {
                    unsafe { owl_hal_delay_ms(tx.gap_ms.into()) };
                }
            }
            error
        }
    };
    send(RadioMessage::Ack, &protocol::ack_body(request, error), &[]);
}

#[no_mangle]
pub extern "C" fn owl_radio_command_task(_arg: *mut c_void) {
    let mut decoder = FrameDecoder::new();
    let mut input = [0; 512];
    loop {
        let got = unsafe { owl_hal_read(input.as_mut_ptr(), input.len()) };
        for &byte in &input[..got] {
            match decoder.push(byte) {
                Some(Ok(message)) => dispatch(message),
                Some(Err(_)) => with_state(|s| increment(&mut s.stats.bad_msg)),
                None => (),
            }
        }
    }
}

#[no_mangle]
pub extern "C" fn owl_radio_uplink_task(_arg: *mut c_void) {
    // SAFETY: the HAL starts this task once. Only this task accesses its buffer;
    // command replies have their own encoder and share the TX mutex at write time.
    static BATCH: Shared<Batch> = Shared(UnsafeCell::new(Batch::new()));
    let batch = unsafe { &mut *BATCH.0.get() };
    let mut encode_us = 0u32;
    loop {
        let mut len = 0;
        let mut item = unsafe { owl_hal_dequeue(&mut len, true) };
        while !item.is_null() {
            // SAFETY: the ring buffer grants exclusive ownership until release.
            let body = unsafe { slice::from_raw_parts(item, len) };
            if let Some((&kind, _)) = body.split_first() {
                if let Some(RadioMessage::Rx | RadioMessage::TxDone) = RadioMessage::from_byte(kind)
                {
                    let mut result = push_uplink(batch, body, &mut encode_us);
                    if result == Ok(false) {
                        flush_uplink(batch, &mut encode_us);
                        result = push_uplink(batch, body, &mut encode_us);
                    }
                    if result != Ok(true) {
                        with_state(|s| increment(&mut s.uplink.errors));
                    }
                }
            }
            // The encoded batch owns these bytes now, so free their RX capacity
            // before the next write. One not-yet-fitting item may remain owned
            // while the previous batch is flushed above.
            unsafe { owl_hal_release(item) };
            if batch.frames() >= BATCH_FRAMES {
                break;
            }
            // No delay to collect a fuller batch: command/idle latency stays low.
            item = unsafe { owl_hal_dequeue(&mut len, false) };
        }
        flush_uplink(batch, &mut encode_us);
    }
}

fn push_uplink(
    batch: &mut Batch,
    message: &[u8],
    encode_us: &mut u32,
) -> Result<bool, wayline_proto::CodecError> {
    let start = unsafe { owl_hal_time_us() };
    let result = batch.push(message);
    *encode_us = encode_us.wrapping_add((unsafe { owl_hal_time_us() } - start) as u32);
    result
}

fn flush_uplink(batch: &mut Batch, encode_us: &mut u32) {
    if batch.frames() == 0 {
        return;
    }
    let (written, wait, write) = unsafe {
        let waiting = owl_hal_time_us();
        owl_hal_tx_lock();
        let _guard = Guard(owl_hal_tx_unlock);
        let writing = owl_hal_time_us();
        let bytes = batch.bytes();
        let written = owl_hal_write(bytes.as_ptr(), bytes.len());
        (
            written,
            (writing - waiting) as u32,
            (owl_hal_time_us() - writing) as u32,
        )
    };
    with_state(|s| s.uplink.record(batch, written, *encode_us, wait, write));
    batch.clear();
    *encode_us = 0;
}

#[no_mangle]
pub extern "C" fn owl_radio_aggregation_task(_arg: *mut c_void) {
    let mut frame = [0u8; aggregation::CONTROL_MAX];
    loop {
        let mut generation = 0;
        // SAFETY: HAL copies at most frame.len() bytes and writes one u32.
        let len = unsafe { owl_hal_ba_dequeue(frame.as_mut_ptr(), frame.len(), &mut generation) };
        let guard = config_guard();
        let now = now_ms();
        let (own, active) =
            with_state(|s| (s.mac, s.host_active(now) && s.filter == FilterMode::Awdl));
        if !active {
            reset_aggregation(&guard);
            continue;
        }
        with_aggregation(&guard, |rx| {
            rx.tick(&own, now, &mut BaHardware);
            if len > 0 && len <= frame.len() && generation == BA_GENERATION.load(Ordering::Relaxed)
            {
                rx.action(&frame[..len], &own, now, &mut BaHardware);
            }
        });
    }
}

#[no_mangle]
pub unsafe extern "C" fn owl_radio_rx(
    kind: u8,
    frame: *const u8,
    len: usize,
    timestamp: u32,
    rssi: i8,
    channel: u8,
    rate: u8,
    sig_mode: u8,
    mcs: u8,
    cwb: u8,
    aggregated: bool,
) {
    if frame.is_null() || len > PROTO_MAX_FRAME {
        return;
    }
    let kind = match kind {
        0 => PacketKind::Management,
        1 => PacketKind::Data,
        2 => PacketKind::Control,
        _ => return,
    };
    // SAFETY: IDF owns this callback buffer and keeps it alive until we return.
    let frame = slice::from_raw_parts(frame, len);
    let now = now_ms();
    let (filter, mac) = with_state(|s| {
        #[cfg(feature = "cardputer-ui")]
        if matches!(kind, PacketKind::Management) {
            s.record_peer(frame, rssi, now);
        }
        let _ = now;
        (s.filter, s.mac)
    });
    if owl_hal_ba_enabled() && filter == FilterMode::Awdl {
        if aggregation::is_action(frame, &mac) {
            if !owl_hal_ba_enqueue(
                frame.as_ptr(),
                frame.len(),
                BA_GENERATION.load(Ordering::Relaxed),
            ) {
                BA_CONTROL_DROPS.fetch_add(1, Ordering::Relaxed);
            }
            return;
        }
        // A Wi-Fi callback must never wait for the configuration task.
        if owl_hal_config_try_lock() {
            let guard = ConfigGuard;
            with_aggregation(&guard, |rx| rx.observe(frame, &mac, now));
        }
    }
    if !state::wanted(filter, kind, frame, &mac) {
        return;
    }
    let mut header = [0; 10];
    header[..4].copy_from_slice(&timestamp.to_le_bytes());
    header[4..].copy_from_slice(&[rssi as u8, channel, rate, sig_mode, mcs, cwb]);
    // Count only aggregated data addressed to this radio; AWDL filtering also
    // forwards other peers' frames, which must not look like our aggregation.
    let aggregated =
        aggregated && matches!(kind, PacketKind::Data) && frame.len() >= 10 && frame[4..10] == mac;
    if aggregated {
        BA_RX.fetch_add(1, Ordering::Relaxed);
    }
    let queued = owl_hal_enqueue(
        RadioMessage::Rx as u8,
        header.as_ptr(),
        header.len(),
        frame.as_ptr(),
        frame.len(),
    );
    if aggregated && !queued {
        BA_RX_DROPS.fetch_add(1, Ordering::Relaxed);
    }
    with_state(|s| {
        increment(if queued {
            &mut s.stats.rx
        } else {
            &mut s.stats.rx_drop
        })
    });
}

#[no_mangle]
pub unsafe extern "C" fn owl_radio_tx_done(acked: bool, destination: *const u8) {
    if destination.is_null() {
        return;
    }
    with_state(|s| {
        increment(if acked {
            &mut s.stats.tx_acked
        } else {
            &mut s.stats.tx_noack
        })
    });
    // Wi-Fi task context: copy telemetry to the bounded uplink ring without
    // waiting for the USB mutex or its TX buffer. Counters above remain exact
    // even if a telemetry report is dropped under backpressure.
    let status = [u8::from(acked)];
    if !owl_hal_enqueue(
        RadioMessage::TxDone as u8,
        status.as_ptr(),
        1,
        destination,
        6,
    ) {
        TX_REPORT_DROPS.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(feature = "cardputer-ui")]
#[no_mangle]
pub extern "C" fn owl_radio_ui_task(_arg: *mut c_void) {
    struct UiBuffer(UnsafeCell<Canvas>);
    // SAFETY: this task is started once by owl_hal_start_tasks. No other context accesses it.
    unsafe impl Sync for UiBuffer {}
    static BUFFER: UiBuffer = UiBuffer(UnsafeCell::new(Canvas::new()));
    let canvas = unsafe { &mut *BUFFER.0.get() };
    UI_STAGE.store(1, Ordering::Relaxed);
    let mut ui = UiState::new(Preferences::from_word(unsafe {
        radio_ui_load_preferences()
    }));
    let mut last_render = 0u32;
    UI_STAGE.store(2, Ordering::Relaxed);
    unsafe { radio_ui_start() };
    ui.start_welcome(now_ms());
    let mut brightness = ui.display_brightness();
    unsafe { radio_ui_set_brightness(brightness) };
    let mut sounds = TaskSounds::default();
    let mut player = Player::default();
    let mut volume = ui.preferences().sound_level();
    let mut style = ui.preferences().sound_style();
    UI_STAGE.store(3, Ordering::Relaxed);
    unsafe { radio_ui_set_volume(volume) };
    loop {
        UI_TICKS.fetch_add(1, Ordering::Relaxed);
        let mut key = 0;
        let mut pressed = false;
        let now = now_ms();
        let feedback = with_state(|s| s.feedback);
        let mut repaint = ui.observe_transfers(&feedback, now);
        repaint |= ui.tick(now);
        UI_STAGE.store(4, Ordering::Relaxed);
        if unsafe { radio_ui_poll_key(&mut key, &mut pressed) } && pressed {
            UI_KEYS.fetch_add(1, Ordering::Relaxed);
            let now = now_ms();
            repaint |= ui.key(key, now);
        }
        // Observe while muted/sleeping as well; status heartbeats never replay a chime.
        let event = sounds.observe(&feedback, now);
        let preview = ui.take_sound_preview();
        let next_volume = ui.preferences().sound_level();
        if volume != next_volume {
            volume = next_volume;
            unsafe { radio_ui_set_volume(volume) };
        }
        let cue = event.or(preview.then_some(Cue::Complete));
        let next_style = ui.preferences().sound_style();
        if style != next_style || cue.is_some() {
            UI_STAGE.store(5, Ordering::Relaxed);
            unsafe { radio_ui_stop_sound() };
            style = next_style;
        }
        if let Some(tone) = player.tick(cue, style, volume > 0, now) {
            UI_STAGE.store(6, Ordering::Relaxed);
            let samples = waveform(tone.style);
            // SAFETY: these immutable samples have static lifetime, including during playback.
            unsafe { radio_ui_tone(tone.hz, tone.duration_ms, samples.as_ptr(), samples.len()) };
        }
        let now = now_ms();
        if ui.should_save(now) {
            UI_STAGE.store(7, Ordering::Relaxed);
            ui.saved(unsafe { radio_ui_save_preferences(ui.preferences().word()) });
            repaint = true;
        }
        let interval = if ui.preferences().motion() { 50 } else { 250 };
        if ui.sleeping() {
            canvas.pause_motion();
        } else if repaint || now.wrapping_sub(last_render) >= interval {
            UI_STAGE.store(8, Ordering::Relaxed);
            let connected = with_state(|s| s.host_active(now));
            ui::render_with_transfers(canvas, &ui, connected, now, &feedback);
            UI_STAGE.store(9, Ordering::Relaxed);
            unsafe { radio_ui_present(canvas.pixels.as_ptr()) };
            UI_FRAMES.fetch_add(1, Ordering::Relaxed);
            last_render = now;
        }
        // Sleep affects only the panel; keyboard polling, persistence and radio tasks continue.
        if brightness != ui.display_brightness() {
            UI_STAGE.store(10, Ordering::Relaxed);
            brightness = ui.display_brightness();
            unsafe { radio_ui_set_brightness(brightness) };
        }
        UI_STAGE.store(11, Ordering::Relaxed);
        unsafe { owl_hal_delay_ms(10) };
    }
}

#[no_mangle]
pub extern "C" fn owl_radio_main() {
    let mut mac = [0; 6];
    let channel = unsafe { owl_hal_default_channel() };
    unsafe {
        owl_hal_init();
        owl_hal_radio_init(mac.as_mut_ptr());
    }
    with_state(|s| {
        s.mac = mac;
        s.channel = channel;
    });
    assert_eq!(unsafe { owl_hal_apply(channel, FilterMode::Awdl as u8) }, 0);
    unsafe { owl_hal_start_tasks() };
    send(RadioMessage::Log, b"owl-radio up (Rust + ESP-IDF)", &[]);
}
