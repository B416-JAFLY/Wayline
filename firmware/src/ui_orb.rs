//! Allocation-free dotted sphere geometry, shared by the device and UI preview.
//! Coordinates use Q14 internally and Q8 pixels at the drawing boundary.
const UNIT: i32 = 1 << 14;
const FULL: u32 = 1 << 16;
const TRANSITION_MS: u32 = 600;
const ROTATION_MS: u32 = 20_000;
const BREATH_MS: u32 = 3_800;
const MAX_FRAME_GAP_MS: u32 = 250;
const CAMERA: i32 = 62_259; // 3.8 radii in Q14.
pub const DOT_COUNT: usize = 192;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dot {
    pub x_q8: i16,
    pub y_q8: i16,
    pub radius_q8: u16,
    /// 0 is the back of the sphere; 255 is the front.
    pub depth: u8,
    /// Includes connection dimming; offline points remain visible.
    pub alpha: u8,
}
impl Dot {
    const ZERO: Self = Self {
        x_q8: 0,
        y_q8: 0,
        radius_q8: 0,
        depth: 0,
        alpha: 0,
    };
}

#[derive(Clone, Copy)]
pub struct OrbMotion {
    rotation_q16: u32,
    breath_q16: u32,
    strength_q16: u32,
    transition_from_q16: u32,
    last_ms: u32,
    breath_offset_q14: i16,
    transition_ms: u16,
    connected: bool,
    seen_time: bool,
    was_visible: bool,
    was_motion: bool,
}

impl Default for OrbMotion {
    fn default() -> Self {
        Self::new()
    }
}

impl OrbMotion {
    /// All fields must stay zero-initialized: Canvas embeds this value in BSS.
    pub const fn new() -> Self {
        Self {
            rotation_q16: 0,
            breath_q16: 0,
            strength_q16: 0,
            transition_from_q16: 0,
            last_ms: 0,
            breath_offset_q14: 0,
            transition_ms: 0,
            connected: false,
            seen_time: false,
            was_visible: false,
            was_motion: false,
        }
    }

    /// Advance visible animation time. Timers wrap safely; hidden views, disabled
    /// motion and gaps over 250 ms never catch up on the next rendered frame.
    pub fn update(&mut self, now: u32, connected: bool, motion: bool, visible: bool) {
        let elapsed = if self.seen_time && self.was_visible && self.was_motion && visible && motion
        {
            let delta = now.wrapping_sub(self.last_ms);
            if delta <= MAX_FRAME_GAP_MS {
                delta
            } else {
                0
            }
        } else {
            0
        };
        self.last_ms = now;
        self.seen_time = true;
        self.was_visible = visible;
        self.was_motion = motion;
        if !visible {
            return;
        }
        let target = if connected { FULL } else { 0 };
        if !motion {
            self.connected = connected;
            self.strength_q16 = target;
            self.transition_from_q16 = target;
            self.transition_ms = TRANSITION_MS as u16;
            // Preserve rotation, breathing phase and current radius even when
            // the connection changes while reduced motion is enabled.
            return;
        }
        if connected != self.connected {
            self.connected = connected;
            self.transition_from_q16 = self.strength_q16;
            self.transition_ms = 0;
        }
        if elapsed == 0 {
            return;
        }
        let before = u32::from(self.transition_ms);
        let transitioning = elapsed.min(TRANSITION_MS - before);
        let after = before + transitioning;
        let movement = transition_area(self.transition_from_q16, target, after)
            - transition_area(self.transition_from_q16, target, before)
            + u64::from(target) * u64::from(elapsed - transitioning);
        self.transition_ms = after as u16;
        self.strength_q16 = if after == TRANSITION_MS {
            target
        } else {
            let start = i64::from(self.transition_from_q16);
            (start + (i64::from(target) - start) * i64::from(smoothstep(after)) / i64::from(FULL))
                as u32
        };
        // Each step is bounded by MAX_FRAME_GAP_MS, so these u32 additions fit.
        self.rotation_q16 = (self.rotation_q16 + movement as u32) % (ROTATION_MS * FULL);
        self.breath_q16 = (self.breath_q16 + movement as u32) % (BREATH_MS * FULL);
        self.breath_offset_q14 = (i64::from(sin_q14((self.breath_q16 / BREATH_MS) as u16))
            * i64::from(self.strength_q16)
            / (10 * i64::from(FULL))) as i16;
    }

    pub fn intensity(&self) -> u32 {
        (self.strength_q16 * 255 + FULL / 2) / FULL
    }

    /// Forget the previous frame time when presentation stops without rendering,
    /// such as manual sleep. Keep the current pose and connection strength.
    pub fn pause(&mut self) {
        self.seen_time = false;
    }

    /// Return back-to-front dots without allocation. Radius is clamped to 0..64
    /// pixels so Q8 coordinates remain representable. At the supported radii
    /// 21 and 25, all pixels fit within 1.2 * radius + 1 around the center.
    pub fn project(&self, radius: i32) -> [Dot; DOT_COUNT] {
        let radius = radius.clamp(0, 64);
        // Initial yaw and tilt live here, keeping the persistent state all zero.
        let yaw = ((self.rotation_q16 / ROTATION_MS) as u16).wrapping_add(3651);
        let yaw_sin = sin_q14(yaw);
        let yaw_cos = sin_q14(yaw.wrapping_add(16384));
        let tilt_sin = sin_q14(2920);
        let tilt_cos = sin_q14(2920 + 16384);
        let scale = UNIT + i32::from(self.breath_offset_q14);
        let visibility = 61 + 194 * self.intensity() / 255;
        let mut dots = [Dot::ZERO; DOT_COUNT];
        for (dot, point) in dots.iter_mut().zip(POINTS.iter()) {
            let [px, py, pz] = point.map(i32::from);
            let x = (px * yaw_cos + pz * yaw_sin) / UNIT;
            let z = (pz * yaw_cos - px * yaw_sin) / UNIT;
            let y = (py * tilt_cos - z * tilt_sin) / UNIT;
            let z = (py * tilt_sin + z * tilt_cos) / UNIT;
            let depth = ((z + UNIT) * 255 / (2 * UNIT)).clamp(0, 255) as u32;
            let scaled_z = z * scale / UNIT;
            let denominator = CAMERA - scaled_z;
            let project = |axis: i32| -> i16 {
                (i64::from(axis) * i64::from(scale) * i64::from(radius) * 256 * i64::from(CAMERA)
                    / (i64::from(UNIT) * i64::from(UNIT) * i64::from(denominator)))
                    as i16
            };
            let point_radius =
                (i64::from(128 + depth * 96 / 255) * i64::from(scale) * i64::from(CAMERA)
                    / (i64::from(UNIT) * i64::from(denominator)))
                .clamp(128, 256) as u16;
            *dot = Dot {
                x_q8: project(x),
                y_q8: project(y),
                radius_q8: if radius == 0 { 0 } else { point_radius },
                depth: depth as u8,
                alpha: ((50 + 205 * depth * depth / (255 * 255)) * visibility / 255) as u8,
            };
        }
        // slice::sort_unstable is available in core and uses no heap allocation.
        dots.sort_unstable_by_key(|dot| dot.depth);
        dots
    }
}

fn smoothstep(ms: u32) -> u32 {
    let t = u64::from(ms);
    let period = u64::from(TRANSITION_MS);
    (t * t * (3 * period - 2 * t) * u64::from(FULL) / (period * period * period)) as u32
}

/// Integral of smoothstep in Q16 milliseconds. Taking differences of this
/// cumulative integral avoids frame-rate-dependent rounding accumulation.
fn transition_area(start: u32, target: u32, ms: u32) -> u64 {
    let t = u64::from(ms);
    let period = u64::from(TRANSITION_MS);
    let integral = t * t * t * (2 * period - t) * u64::from(FULL) / (2 * period * period * period);
    (i64::from(start) * i64::from(ms)
        + (i64::from(target) - i64::from(start)) * integral as i64 / i64::from(FULL)) as u64
}

/// A turn is 65536 units. A 65-entry quarter-wave table with linear interpolation
/// provides rotation without floating point, libm or runtime initialization.
fn sin_q14(phase: u16) -> i32 {
    let quadrant = phase >> 14;
    let position = usize::from(phase & 0x3fff);
    let position = if quadrant & 1 == 1 {
        16384 - position
    } else {
        position
    };
    let index = position / 256;
    let fraction = (position % 256) as i32;
    let value = if index == 64 {
        i32::from(SIN_QUARTER[64])
    } else {
        (i32::from(SIN_QUARTER[index]) * (256 - fraction)
            + i32::from(SIN_QUARTER[index + 1]) * fraction)
            / 256
    };
    if quadrant >= 2 {
        -value
    } else {
        value
    }
}

const SIN_QUARTER: [i16; 65] = [
    0, 402, 804, 1205, 1606, 2006, 2404, 2801, 3196, 3590, 3981, 4370, 4756, 5139, 5520, 5897,
    6270, 6639, 7005, 7366, 7723, 8076, 8423, 8765, 9102, 9434, 9760, 10080, 10394, 10702, 11003,
    11297, 11585, 11866, 12140, 12406, 12665, 12916, 13160, 13395, 13623, 13842, 14053, 14256,
    14449, 14635, 14811, 14978, 15137, 15286, 15426, 15557, 15679, 15791, 15893, 15986, 16069,
    16143, 16207, 16261, 16305, 16340, 16364, 16379, 16384,
];

// Unit sphere, Q14. Twelve latitude bands centered between the poles; each
// band's population is round(25 * sin(latitude)), with alternate rows staggered.
// Stored in flash once; no point generation, trigonometry or heap is needed at boot.
static POINTS: [[i16; 3]; DOT_COUNT] = [
    [2139, 16244, 0],
    [-1069, 16244, 1852],
    [-1069, 16244, -1852],
    [5963, 15137, 1938],
    [3685, 15137, 5072],
    [0, 15137, 6270],
    [-3685, 15137, 5072],
    [-5963, 15137, 1938],
    [-5963, 15137, -1938],
    [-3685, 15137, -5072],
    [0, 15137, -6270],
    [3685, 15137, -5072],
    [5963, 15137, -1938],
    [9974, 12998, 0],
    [9112, 12998, 4057],
    [6674, 12998, 7412],
    [3082, 12998, 9486],
    [-1043, 12998, 9919],
    [-4987, 12998, 8638],
    [-8069, 12998, 5863],
    [-9756, 12998, 2074],
    [-9756, 12998, -2074],
    [-8069, 12998, -5863],
    [-4987, 12998, -8638],
    [-1043, 12998, -9919],
    [3082, 12998, -9486],
    [6674, 12998, -7412],
    [9112, 12998, -4057],
    [12838, 9974, 2033],
    [11582, 9974, 5901],
    [9191, 9974, 9191],
    [5901, 9974, 11582],
    [2033, 9974, 12838],
    [-2033, 9974, 12838],
    [-5901, 9974, 11582],
    [-9191, 9974, 9191],
    [-11582, 9974, 5901],
    [-12838, 9974, 2033],
    [-12838, 9974, -2033],
    [-11582, 9974, -5901],
    [-9191, 9974, -9191],
    [-5901, 9974, -11582],
    [-2033, 9974, -12838],
    [2033, 9974, -12838],
    [5901, 9974, -11582],
    [9191, 9974, -9191],
    [11582, 9974, -5901],
    [12838, 9974, -2033],
    [15137, 6270, 0],
    [14576, 6270, 4084],
    [12933, 6270, 7865],
    [10332, 6270, 11063],
    [6964, 6270, 13440],
    [3080, 6270, 14820],
    [-1033, 6270, 15102],
    [-5069, 6270, 14263],
    [-8729, 6270, 12366],
    [-11742, 6270, 9553],
    [-13884, 6270, 6031],
    [-14996, 6270, 2061],
    [-14996, 6270, -2061],
    [-13884, 6270, -6031],
    [-11742, 6270, -9553],
    [-8729, 6270, -12366],
    [-5069, 6270, -14263],
    [-1033, 6270, -15102],
    [3080, 6270, -14820],
    [6964, 6270, -13440],
    [10332, 6270, -11063],
    [12933, 6270, -7865],
    [14576, 6270, -4084],
    [16116, 2139, 2036],
    [15103, 2139, 5980],
    [13142, 2139, 9548],
    [10354, 2139, 12516],
    [6916, 2139, 14698],
    [3044, 2139, 15956],
    [-1020, 2139, 16212],
    [-5020, 2139, 15449],
    [-8704, 2139, 13715],
    [-11841, 2139, 11120],
    [-14235, 2139, 7826],
    [-15734, 2139, 4040],
    [-16244, 2139, 0],
    [-15734, 2139, -4040],
    [-14235, 2139, -7826],
    [-11841, 2139, -11120],
    [-8704, 2139, -13715],
    [-5020, 2139, -15449],
    [-1020, 2139, -16212],
    [3044, 2139, -15956],
    [6916, 2139, -14698],
    [10354, 2139, -12516],
    [13142, 2139, -9548],
    [15103, 2139, -5980],
    [16116, 2139, -2036],
    [16244, -2139, 0],
    [15734, -2139, 4040],
    [14235, -2139, 7826],
    [11841, -2139, 11120],
    [8704, -2139, 13715],
    [5020, -2139, 15449],
    [1020, -2139, 16212],
    [-3044, -2139, 15956],
    [-6916, -2139, 14698],
    [-10354, -2139, 12516],
    [-13142, -2139, 9548],
    [-15103, -2139, 5980],
    [-16116, -2139, 2036],
    [-16116, -2139, -2036],
    [-15103, -2139, -5980],
    [-13142, -2139, -9548],
    [-10354, -2139, -12516],
    [-6916, -2139, -14698],
    [-3044, -2139, -15956],
    [1020, -2139, -16212],
    [5020, -2139, -15449],
    [8704, -2139, -13715],
    [11841, -2139, -11120],
    [14235, -2139, -7826],
    [15734, -2139, -4040],
    [14996, -6270, 2061],
    [13884, -6270, 6031],
    [11742, -6270, 9553],
    [8729, -6270, 12366],
    [5069, -6270, 14263],
    [1033, -6270, 15102],
    [-3080, -6270, 14820],
    [-6964, -6270, 13440],
    [-10332, -6270, 11063],
    [-12933, -6270, 7865],
    [-14576, -6270, 4084],
    [-15137, -6270, 0],
    [-14576, -6270, -4084],
    [-12933, -6270, -7865],
    [-10332, -6270, -11063],
    [-6964, -6270, -13440],
    [-3080, -6270, -14820],
    [1033, -6270, -15102],
    [5069, -6270, -14263],
    [8729, -6270, -12366],
    [11742, -6270, -9553],
    [13884, -6270, -6031],
    [14996, -6270, -2061],
    [12998, -9974, 0],
    [12362, -9974, 4017],
    [10516, -9974, 7640],
    [7640, -9974, 10516],
    [4017, -9974, 12362],
    [0, -9974, 12998],
    [-4017, -9974, 12362],
    [-7640, -9974, 10516],
    [-10516, -9974, 7640],
    [-12362, -9974, 4017],
    [-12998, -9974, 0],
    [-12362, -9974, -4017],
    [-10516, -9974, -7640],
    [-7640, -9974, -10516],
    [-4017, -9974, -12362],
    [0, -9974, -12998],
    [4017, -9974, -12362],
    [7640, -9974, -10516],
    [10516, -9974, -7640],
    [12362, -9974, -4017],
    [9756, -12998, 2074],
    [8069, -12998, 5863],
    [4987, -12998, 8638],
    [1043, -12998, 9919],
    [-3082, -12998, 9486],
    [-6674, -12998, 7412],
    [-9112, -12998, 4057],
    [-9974, -12998, 0],
    [-9112, -12998, -4057],
    [-6674, -12998, -7412],
    [-3082, -12998, -9486],
    [1043, -12998, -9919],
    [4987, -12998, -8638],
    [8069, -12998, -5863],
    [9756, -12998, -2074],
    [6270, -15137, 0],
    [5072, -15137, 3685],
    [1938, -15137, 5963],
    [-1938, -15137, 5963],
    [-5072, -15137, 3685],
    [-6270, -15137, 0],
    [-5072, -15137, -3685],
    [-1938, -15137, -5963],
    [1938, -15137, -5963],
    [5072, -15137, -3685],
    [1069, -16244, 1852],
    [-2139, -16244, 0],
    [1069, -16244, -1852],
];

#[cfg(test)]
mod tests {
    use super::*;

    fn advance(motion: &mut OrbMotion, start: u32, duration: u32, fps: u32, connected: bool) {
        motion.update(start, connected, true, true);
        let frames = duration * fps / 1000;
        for frame in 1..=frames {
            motion.update(
                start.wrapping_add(frame * duration / frames),
                connected,
                true,
                true,
            );
        }
    }

    fn positions(dots: [Dot; DOT_COUNT]) -> [(i16, i16, u16); DOT_COUNT] {
        dots.map(|dot| (dot.x_q8, dot.y_q8, dot.radius_q8))
    }

    #[test]
    fn state_starts_zero_and_projection_buffer_stays_small() {
        let motion = OrbMotion::new();
        assert_eq!(
            (motion.rotation_q16, motion.breath_q16, motion.strength_q16),
            (0, 0, 0)
        );
        assert_eq!((motion.transition_from_q16, motion.last_ms), (0, 0));
        assert_eq!((motion.breath_offset_q14, motion.transition_ms), (0, 0));
        assert!(
            !motion.connected && !motion.seen_time && !motion.was_visible && !motion.was_motion
        );
        assert_eq!(core::mem::size_of::<Dot>(), 8);
        assert!(core::mem::size_of::<[Dot; DOT_COUNT]>() < 2048);
        assert!(core::mem::size_of::<OrbMotion>() <= 32);
    }

    #[test]
    fn points_have_uniform_radius_and_no_crowded_poles() {
        for (index, point) in POINTS.iter().enumerate() {
            let [x, y, z] = point.map(i64::from);
            let norm = x * x + y * y + z * z;
            assert!((norm - i64::from(UNIT) * i64::from(UNIT)).abs() < 30_000);
            let nearest = POINTS
                .iter()
                .enumerate()
                .filter(|(other, _)| *other != index)
                .map(|(_, other)| {
                    point
                        .iter()
                        .zip(other)
                        .map(|(&a, &b)| {
                            let delta = i64::from(a) - i64::from(b);
                            delta * delta
                        })
                        .sum::<i64>()
                })
                .min()
                .unwrap();
            assert!(
                (10_000_000..22_000_000).contains(&nearest),
                "nearest: {nearest}"
            );
        }
    }

    #[test]
    fn sine_is_bounded_symmetric_and_continuous_at_quadrants() {
        for phase in 0..=u16::MAX {
            let value = sin_q14(phase);
            assert!((-UNIT..=UNIT).contains(&value));
            assert_eq!(value, -sin_q14(phase.wrapping_add(32768)));
            assert!((value - sin_q14(phase.wrapping_add(1))).abs() <= 3);
        }
    }

    #[test]
    fn projection_is_bounded_sorted_and_retains_visible_offline_dots() {
        let mut motion = OrbMotion::new();
        for frame in 0..=500 {
            motion.update(frame * 50, true, true, true);
            for radius in [21, 25] {
                let dots = motion.project(radius);
                assert!(dots.windows(2).all(|pair| pair[0].depth <= pair[1].depth));
                let limit = i64::from(radius * 6 * 256 / 5 + 256);
                for dot in dots {
                    let point_limit = limit - i64::from(dot.radius_q8);
                    assert!(
                        i64::from(dot.x_q8).pow(2) + i64::from(dot.y_q8).pow(2)
                            < point_limit * point_limit
                    );
                    assert!((128..=256).contains(&dot.radius_q8));
                    assert!(dot.alpha > 0);
                }
            }
        }
        for radius in [i32::MIN, -1, 0, 64, i32::MAX] {
            let _ = motion.project(radius);
        }
        assert!(OrbMotion::new().project(25).iter().all(|dot| dot.alpha > 0));
    }

    #[test]
    fn equivalent_elapsed_time_and_timer_wrap_produce_the_same_pose() {
        let mut regular = OrbMotion::new();
        let mut fast = OrbMotion::new();
        let mut wrapping = OrbMotion::new();
        let mut elapsed = 0;
        let wrap_start = u32::MAX - 100;
        for (duration, connected) in [(7000, true), (1000, false), (4000, true), (1000, false)] {
            advance(&mut regular, elapsed, duration, 20, connected);
            advance(&mut fast, elapsed, duration, 60, connected);
            advance(
                &mut wrapping,
                wrap_start.wrapping_add(elapsed),
                duration,
                20,
                connected,
            );
            assert_eq!(regular.project(25), fast.project(25));
            assert_eq!(regular.project(25), wrapping.project(25));
            assert_eq!(regular.intensity(), fast.intensity());
            elapsed += duration;
        }
    }

    #[test]
    fn reduced_motion_freezes_geometry_even_if_connection_changes() {
        let mut motion = OrbMotion::new();
        advance(&mut motion, 0, 2000, 20, true);
        let pose = positions(motion.project(25));
        motion.update(2050, false, false, true);
        assert_eq!(motion.intensity(), 0);
        assert_eq!(positions(motion.project(25)), pose);
        motion.update(2100, true, false, true);
        assert_eq!(motion.intensity(), 255);
        motion.update(20_000, true, false, true);
        assert_eq!(positions(motion.project(25)), pose);
    }

    #[test]
    fn hiding_or_a_large_frame_gap_never_catches_up() {
        let mut motion = OrbMotion::new();
        advance(&mut motion, 0, 2000, 20, true);
        let pose = motion.project(25);
        motion.update(2050, true, true, false);
        motion.update(20_000, true, true, true);
        assert_eq!(motion.project(25), pose);
        motion.update(40_000, true, true, true);
        assert_eq!(motion.project(25), pose);
        motion.update(40_050, true, true, true);
        assert_ne!(motion.project(25), pose);
    }

    #[test]
    fn explicit_pause_preserves_pose_even_after_a_short_sleep() {
        let mut motion = OrbMotion::new();
        advance(&mut motion, 0, 2000, 20, true);
        let pose = motion.project(25);
        motion.pause();
        motion.update(2020, true, true, true);
        assert_eq!(motion.project(25), pose);
        motion.update(2070, true, true, true);
        assert_ne!(motion.project(25), pose);
    }

    #[test]
    fn disconnect_eases_to_a_stationary_pose_without_resetting_rotation() {
        let mut motion = OrbMotion::new();
        advance(&mut motion, 0, 2000, 20, true);
        let pose = motion.project(25);
        motion.update(2000, false, true, true);
        assert_eq!(motion.project(25), pose);
        advance(&mut motion, 2000, 300, 20, false);
        assert!((127..=128).contains(&motion.intensity()));
        advance(&mut motion, 2300, 300, 20, false);
        assert_eq!(motion.intensity(), 0);
        assert_ne!(motion.rotation_q16, 0);
        let settled = motion.project(25);
        advance(&mut motion, 2600, 2000, 20, false);
        assert_eq!(motion.project(25), settled);
    }
}
