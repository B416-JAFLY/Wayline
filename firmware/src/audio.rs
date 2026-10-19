//! Task chimes and note timing, independent of the speaker driver and frame rate.
use crate::feedback::Feedback;
use omdrop_proto::transfer::Phase;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Cue {
    Request,
    Cancelled,
    Complete,
    Failed,
}

/// Stable IDs persisted in UI preferences. Waveforms are unsigned 8-bit single cycles.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum SoundStyle {
    #[default]
    Soft = 0,
    EightBit = 1,
    Chime = 2,
    Minimal = 3,
}
impl SoundStyle {
    pub fn from_id(id: u8) -> Option<Self> {
        match id {
            0 => Some(Self::Soft),
            1 => Some(Self::EightBit),
            2 => Some(Self::Chime),
            3 => Some(Self::Minimal),
            _ => None,
        }
    }
    pub fn next(self) -> Self {
        Self::from_id((self as u8 + 1) % 4).unwrap_or_default()
    }
}

const SINE: [i16; 32] = [
    0, 25, 49, 71, 90, 106, 117, 125, 127, 125, 117, 106, 90, 71, 49, 25, 0, -25, -49, -71, -90,
    -106, -117, -125, -127, -125, -117, -106, -90, -71, -49, -25,
];
const fn waveforms() -> [[u8; 32]; 4] {
    let mut waves = [[128; 32]; 4];
    let mut i = 0;
    while i < 32 {
        // Similar RMS levels, zero DC and headroom avoid a loud jump when selecting 8-bit.
        waves[0][i] = (128 + SINE[i] * 3 / 4) as u8;
        waves[1][i] = if i < 16 { 195 } else { 61 };
        waves[2][i] = (128 + (SINE[i] * 3 + SINE[(i * 3) % 32]) / 5) as u8;
        let triangle = if i < 8 {
            i as i16 * 14
        } else if i < 24 {
            (16 - i as i16) * 14
        } else {
            (i as i16 - 32) * 14
        };
        waves[3][i] = (128 + triangle) as u8;
        i += 1;
    }
    waves
}
static WAVES: [[u8; 32]; 4] = waveforms();
/// Static storage outlives asynchronous driver playback; never hand the driver stack samples.
pub fn waveform(style: SoundStyle) -> &'static [u8; 32] {
    &WAVES[style as usize]
}

#[derive(Default)]
pub struct TaskSounds {
    ids: [u64; 2],
    announced: [u8; 2],
    last: Option<(Cue, u32)>,
}
impl TaskSounds {
    /// Always observe, even when muted, so enabling sound cannot replay old results.
    pub fn observe(&mut self, feedback: &Feedback, now: u32) -> Option<Cue> {
        let mut cue = None;
        for (direction, lane) in feedback.lanes.iter().enumerate() {
            let Some(value) = lane.visible(now) else {
                continue;
            };
            if self.ids[direction] != value.id {
                self.ids[direction] = value.id;
                self.announced[direction] = 0;
            }
            let event = match value.phase {
                Phase::Waiting if direction == 1 => Cue::Request,
                Phase::Complete => Cue::Complete,
                Phase::Failed => Cue::Failed,
                Phase::Cancelled => Cue::Cancelled,
                _ => continue,
            };
            let bit = 1 << event as u8;
            if self.announced[direction] & bit == 0 {
                self.announced[direction] |= bit;
                cue = Some(cue.map_or(event, |previous: Cue| previous.max(event)));
            }
        }
        if let Some(event) = cue {
            // Coalesce near-simultaneous results from the two lanes.
            if self
                .last
                .is_some_and(|(last, at)| last == event && now.wrapping_sub(at) < 500)
            {
                return None;
            }
            self.last = Some((event, now));
        }
        cue
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tone {
    pub hz: u16,
    pub duration_ms: u16,
    pub style: SoundStyle,
}

#[derive(Clone, Copy)]
struct Sequence {
    notes: [Tone; 3],
    len: usize,
    gap: u16,
}
fn sequence(style: SoundStyle, cue: Cue) -> Sequence {
    let (notes, gap) = match style {
        SoundStyle::Soft => (
            match cue {
                Cue::Request => [(880, 90), (1047, 110), (0, 0)],
                Cue::Complete => [(1047, 90), (1319, 110), (0, 0)],
                Cue::Failed => [(659, 90), (494, 110), (0, 0)],
                Cue::Cancelled => [(523, 90), (0, 0), (0, 0)],
            },
            40,
        ),
        SoundStyle::EightBit => (
            match cue {
                Cue::Request => [(784, 70), (1047, 70), (1175, 100)],
                Cue::Complete => [(1047, 60), (1319, 60), (1568, 100)],
                Cue::Failed => [(784, 70), (622, 70), (392, 130)],
                Cue::Cancelled => [(659, 60), (523, 90), (0, 0)],
            },
            20,
        ),
        SoundStyle::Chime => (
            match cue {
                Cue::Request => [(1047, 130), (1568, 180), (0, 0)],
                Cue::Complete => [(1319, 120), (1760, 220), (0, 0)],
                Cue::Failed => [(880, 110), (659, 180), (0, 0)],
                Cue::Cancelled => [(784, 120), (0, 0), (0, 0)],
            },
            45,
        ),
        SoundStyle::Minimal => (
            match cue {
                Cue::Request => [(880, 65), (0, 0), (0, 0)],
                Cue::Complete => [(1047, 70), (0, 0), (0, 0)],
                Cue::Failed => [(440, 100), (0, 0), (0, 0)],
                Cue::Cancelled => [(523, 45), (0, 0), (0, 0)],
            },
            0,
        ),
    };
    Sequence {
        len: notes.iter().filter(|(hz, _)| *hz != 0).count(),
        notes: notes.map(|(hz, duration_ms)| Tone {
            hz,
            duration_ms,
            style,
        }),
        gap,
    }
}

#[derive(Default)]
pub struct Player {
    sequence: Option<Sequence>,
    index: usize,
    due: u32,
    style: SoundStyle,
}
impl Player {
    /// No sleeps: the UI loop submits each short note to the driver's playback task.
    pub fn tick(
        &mut self,
        cue: Option<Cue>,
        style: SoundStyle,
        audible: bool,
        now: u32,
    ) -> Option<Tone> {
        if style != self.style {
            self.sequence = None;
            self.style = style;
        }
        if !audible {
            self.sequence = None;
            return None;
        }
        if let Some(cue) = cue {
            self.sequence = Some(sequence(style, cue));
            self.index = 0;
            self.due = now;
        }
        if let Some(sequence) = self.sequence {
            let age = now.wrapping_sub(self.due);
            if age < 0x8000_0000 {
                if age >= 250 {
                    self.sequence = None;
                    return None;
                }
                let tone = sequence.notes[self.index];
                self.index += 1;
                self.due = now.wrapping_add(u32::from(tone.duration_ms) + u32::from(sequence.gap));
                if self.index == sequence.len {
                    self.sequence = None;
                }
                return Some(tone);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omdrop_proto::transfer::{Transfer, FRAME_LEN, HEADER_LEN};
    fn set(feedback: &mut Feedback, id: u64, direction: u8, phase: Phase, now: u32) {
        let mut bytes = [0; FRAME_LEN];
        bytes[..HEADER_LEN].copy_from_slice(
            &Transfer {
                id,
                phase,
                ..Transfer::idle(direction)
            }
            .encode(),
        );
        assert!(feedback.apply(&bytes, now));
    }
    #[test]
    fn only_new_events_chime_and_muted_events_do_not_replay() {
        let mut sounds = TaskSounds::default();
        let mut state = Feedback::new();
        set(&mut state, 1, 0, Phase::Preparing, 0);
        assert_eq!(sounds.observe(&state, 0), None);
        set(&mut state, 2, 1, Phase::Waiting, 10);
        assert_eq!(sounds.observe(&state, 10), Some(Cue::Request));
        assert_eq!(sounds.observe(&state, 1000), None);
        set(&mut state, 1, 0, Phase::Complete, 1100);
        set(&mut state, 2, 1, Phase::Complete, 1100);
        assert_eq!(sounds.observe(&state, 1100), Some(Cue::Complete));
        // This result would be consumed silently while muted.
        assert_eq!(sounds.observe(&state, 1200), None);
        set(&mut state, 3, 0, Phase::Preparing, 2000);
        assert_eq!(sounds.observe(&state, 2000), None);
        assert_eq!(sounds.observe(&state, 10_000), Some(Cue::Failed));
        assert_eq!(sounds.observe(&state, 10_001), None);
    }
    #[test]
    fn notes_are_nonblocking_wrap_safe_and_muting_cancels_the_tail() {
        let mut player = Player::default();
        let style = SoundStyle::Soft;
        let start = u32::MAX - 50;
        assert_eq!(
            player
                .tick(Some(Cue::Complete), style, true, start)
                .unwrap()
                .hz,
            1047
        );
        assert_eq!(
            player.tick(None, style, true, start.wrapping_add(129)),
            None
        );
        assert_eq!(
            player
                .tick(None, style, true, start.wrapping_add(130))
                .unwrap()
                .hz,
            1319
        );
        assert_eq!(
            player.tick(None, style, true, start.wrapping_add(140)),
            None
        );
        player.tick(Some(Cue::Failed), style, true, 2000);
        assert_eq!(player.tick(None, style, false, 2010), None);
        assert_eq!(player.tick(None, style, true, 2140), None);
        assert_eq!(player.tick(Some(Cue::Complete), style, false, 3000), None);
        assert_eq!(player.tick(None, style, true, 3140), None);
    }

    #[test]
    fn waves_are_distinct_centered_and_have_comparable_energy() {
        let mut energies = [0i32; 4];
        for (index, wave) in WAVES.iter().enumerate() {
            assert_eq!(wave.iter().map(|x| u32::from(*x)).sum::<u32>(), 128 * 32);
            assert!(wave.iter().all(|x| (16..=240).contains(x)));
            energies[index] = wave.iter().map(|x| (i32::from(*x) - 128).pow(2)).sum();
            for other in &WAVES[..index] {
                assert_ne!(wave, other);
            }
        }
        // Energy ratio below 2 means RMS levels differ by less than 3 dB.
        assert!(energies.iter().max().unwrap() < &(2 * energies.iter().min().unwrap()));
        assert!(waveform(SoundStyle::EightBit)[..16]
            .iter()
            .all(|x| *x == 195));
    }

    #[test]
    fn eight_bit_arpeggio_stops_on_style_change_mute_or_late_tick() {
        let mut player = Player::default();
        let style = SoundStyle::EightBit;
        assert_eq!(
            player.tick(Some(Cue::Complete), style, true, 0).unwrap().hz,
            1047
        );
        assert_eq!(player.tick(None, style, true, 79), None);
        assert_eq!(player.tick(None, style, true, 80).unwrap().hz, 1319);
        assert_eq!(player.tick(None, style, true, 160).unwrap().hz, 1568);
        assert_eq!(player.tick(None, style, true, 280), None);
        player.tick(Some(Cue::Request), style, true, 1000);
        assert_eq!(player.tick(None, SoundStyle::Minimal, true, 1050), None);
        assert_eq!(player.tick(None, style, true, 1090), None);
        player.tick(Some(Cue::Failed), style, true, 2000);
        assert_eq!(player.tick(None, style, false, 2010), None);
        assert_eq!(player.tick(None, style, true, 2200), None);
        player.tick(Some(Cue::Complete), style, true, 3000);
        assert_eq!(player.tick(None, style, true, 3330), None);
        assert_eq!(player.tick(None, style, true, 3340), None);
        let tone = player
            .tick(Some(Cue::Cancelled), SoundStyle::Minimal, true, 4000)
            .unwrap();
        assert_eq!(tone.duration_ms, 45);
        assert_eq!(tone.style, SoundStyle::Minimal);
        assert_eq!(player.tick(None, SoundStyle::Minimal, true, 4050), None);
    }
}
