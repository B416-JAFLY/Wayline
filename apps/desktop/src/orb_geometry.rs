//! Geometry and time-based motion for the sharing card's dotted sphere.
use std::{f64::consts::TAU, sync::OnceLock, time::Duration};

const TRANSITION_SECONDS: f64 = 0.6;
const ROTATION_SECONDS: f64 = 20.0;
const BREATH_SECONDS: f64 = 3.8;
const LATITUDE_BANDS: usize = 19;
const EQUATOR_SAMPLES: usize = 38;
const CAMERA_DISTANCE: f64 = 3.8;

#[derive(Debug, Clone, Copy)]
pub struct Dot {
    /// Projected position in units of the sphere's resting radius.
    pub x: f32,
    pub y: f32,
    /// Projected diameter in the same units; includes breathing.
    pub diameter: f32,
    /// Rotated sphere depth, from -1 (back) to 1 (front).
    pub depth: f32,
    pub opacity: f32,
}

pub struct OrbFrame {
    /// Back-to-front order. The entire frame fits within radius 1.2.
    pub dots: Vec<Dot>,
    /// Smooth connection strength, from 0 (offline) to 1 (ready).
    pub intensity: f32,
}

pub struct OrbMotion {
    connected: bool,
    intensity: f64,
    transition_from: f64,
    transition_elapsed: f64,
    rotation: f64,
    breath: f64,
}

impl Default for OrbMotion {
    fn default() -> Self {
        Self::new()
    }
}

impl OrbMotion {
    pub fn new() -> Self {
        Self {
            connected: false,
            intensity: 0.0,
            transition_from: 0.0,
            transition_elapsed: TRANSITION_SECONDS,
            rotation: 0.35,
            breath: 0.0,
        }
    }

    /// Advance visible animation time. Hidden views can stop calling this method.
    /// Connection changes ease over 600 ms, retaining the current orientation.
    pub fn advance(&mut self, delta: Duration, connected: bool) {
        if connected != self.connected {
            self.connected = connected;
            self.transition_from = self.intensity;
            self.transition_elapsed = 0.0;
        }
        let seconds = delta.as_secs_f64();
        let target = f64::from(self.connected);
        let remaining = (TRANSITION_SECONDS - self.transition_elapsed).max(0.0);
        let transitioning = seconds.min(remaining);
        let before = self.transition_elapsed / TRANSITION_SECONDS;
        self.transition_elapsed += transitioning;
        let after = self.transition_elapsed / TRANSITION_SECONDS;

        // Integrate smoothstep analytically: the resulting pose depends on elapsed
        // time rather than the display refresh rate or the number of frames.
        let movement = self.transition_from * transitioning
            + (target - self.transition_from)
                * TRANSITION_SECONDS
                * (smoothstep_integral(after) - smoothstep_integral(before))
            + target * (seconds - transitioning);
        self.intensity = if self.transition_elapsed >= TRANSITION_SECONDS {
            target
        } else {
            self.transition_from + (target - self.transition_from) * smoothstep(after)
        };
        self.rotation = (self.rotation + movement * TAU / ROTATION_SECONDS).rem_euclid(TAU);
        self.breath = (self.breath + movement * TAU / BREATH_SECONDS).rem_euclid(TAU);
    }

    pub fn needs_animation(&self) -> bool {
        self.connected || self.transition_elapsed < TRANSITION_SECONDS
    }

    pub fn frame(&self) -> OrbFrame {
        let scale = 1.0 + 0.10 * self.intensity * self.breath.sin();
        let (yaw_sin, yaw_cos) = self.rotation.sin_cos();
        let (tilt_sin, tilt_cos) = 0.28_f64.sin_cos();
        let visibility = 0.38 + 0.62 * self.intensity;
        let mut dots: Vec<_> = sphere_points()
            .iter()
            .map(|point| {
                let x = point.x * yaw_cos + point.z * yaw_sin;
                let z = point.z * yaw_cos - point.x * yaw_sin;
                let y = point.y * tilt_cos - z * tilt_sin;
                let z = point.y * tilt_sin + z * tilt_cos;
                let front = (z + 1.0) * 0.5;
                let perspective = CAMERA_DISTANCE / (CAMERA_DISTANCE - z * scale);
                Dot {
                    x: (x * scale * perspective) as f32,
                    y: (y * scale * perspective) as f32,
                    diameter: ((0.020 + 0.012 * front) * perspective * scale).clamp(0.017, 0.049)
                        as f32,
                    depth: z as f32,
                    opacity: (visibility * (0.19 + 0.81 * front.powf(1.6))) as f32,
                }
            })
            .collect();
        dots.sort_unstable_by(|a, b| a.depth.total_cmp(&b.depth));
        OrbFrame {
            dots,
            intensity: self.intensity as f32,
        }
    }
}

fn smoothstep(t: f64) -> f64 {
    t * t * (3.0 - 2.0 * t)
}

fn smoothstep_integral(t: f64) -> f64 {
    t * t * t * (1.0 - 0.5 * t)
}

struct SpherePoint {
    x: f64,
    y: f64,
    z: f64,
}

fn sphere_points() -> &'static [SpherePoint] {
    static POINTS: OnceLock<Vec<SpherePoint>> = OnceLock::new();
    POINTS.get_or_init(|| {
        let mut points = Vec::with_capacity(512);
        for row in 0..LATITUDE_BANDS {
            let latitude = (row as f64 + 0.5) * std::f64::consts::PI / LATITUDE_BANDS as f64;
            let (ring_radius, y) = latitude.sin_cos();
            // Equal latitude spacing and circumference-based counts keep the
            // polar rings as open as the equator. Alternate rows are staggered.
            let count = (EQUATOR_SAMPLES as f64 * ring_radius).round().max(1.0) as usize;
            let offset = if row % 2 == 0 { 0.0 } else { 0.5 };
            for column in 0..count {
                let longitude = (column as f64 + offset) * TAU / count as f64;
                let (sin, cos) = longitude.sin_cos();
                points.push(SpherePoint {
                    x: ring_radius * cos,
                    y,
                    z: ring_radius * sin,
                });
            }
        }
        points
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn advance_steps(motion: &mut OrbMotion, seconds: u64, fps: u32, connected: bool) {
        let total_nanos = seconds * 1_000_000_000;
        let frames = seconds * fps as u64;
        let mut previous = 0;
        for frame in 1..=frames {
            let next = frame * total_nanos / frames;
            motion.advance(Duration::from_nanos(next - previous), connected);
            previous = next;
        }
    }

    fn assert_same_pose(left: &OrbMotion, right: &OrbMotion) {
        assert!((left.rotation - right.rotation).abs() < 1e-10);
        assert!((left.breath - right.breath).abs() < 1e-10);
        assert!((left.intensity - right.intensity).abs() < 1e-10);
    }

    #[test]
    fn sphere_has_bounded_even_spacing_without_crowded_poles() {
        let points = sphere_points();
        assert!((420..=520).contains(&points.len()));
        assert!(std::ptr::eq(points, sphere_points()));
        for (index, point) in points.iter().enumerate() {
            let radius = point.x * point.x + point.y * point.y + point.z * point.z;
            assert!((radius - 1.0).abs() < 1e-12);
            let closest = points
                .iter()
                .enumerate()
                .filter(|(other, _)| *other != index)
                .map(|(_, other)| {
                    ((point.x - other.x).powi(2)
                        + (point.y - other.y).powi(2)
                        + (point.z - other.z).powi(2))
                    .sqrt()
                })
                .fold(f64::INFINITY, f64::min);
            assert!((0.13..0.19).contains(&closest), "spacing: {closest}");
        }
    }

    #[test]
    fn projected_frames_are_finite_bounded_and_sorted_back_to_front() {
        let mut motion = OrbMotion::new();
        for _ in 0..240 {
            motion.advance(Duration::from_millis(100), true);
            let frame = motion.frame();
            assert!((0.0..=1.0).contains(&frame.intensity));
            assert!(
                frame
                    .dots
                    .windows(2)
                    .all(|pair| pair[0].depth <= pair[1].depth)
            );
            for dot in frame.dots {
                assert!(dot.x.is_finite() && dot.y.is_finite());
                assert!(dot.x.hypot(dot.y) + dot.diameter / 2.0 < 1.2);
                assert!((-1.0..=1.0).contains(&dot.depth));
                assert!((0.017..=0.049).contains(&dot.diameter));
                assert!((0.0..=1.0).contains(&dot.opacity));
            }
        }
    }

    #[test]
    fn elapsed_time_produces_the_same_pose_at_different_frame_rates() {
        let mut low = OrbMotion::new();
        let mut high = OrbMotion::new();
        let mut single = OrbMotion::new();
        for (seconds, connected) in [(9, true), (2, false), (14, true), (1, false)] {
            advance_steps(&mut low, seconds, 30, connected);
            advance_steps(&mut high, seconds, 144, connected);
            single.advance(Duration::from_secs(seconds), connected);
            assert_same_pose(&low, &high);
            assert_same_pose(&low, &single);
        }
    }

    #[test]
    fn disconnect_settles_and_leaves_a_visible_stationary_sphere() {
        let mut motion = OrbMotion::new();
        motion.advance(Duration::from_secs(4), true);
        let rotation = motion.rotation;
        motion.advance(Duration::ZERO, false);
        assert_eq!(motion.rotation, rotation);
        assert!(motion.needs_animation());
        motion.advance(Duration::from_millis(300), false);
        assert!((motion.intensity - 0.5).abs() < 1e-12);
        motion.advance(Duration::from_millis(300), false);
        assert!(!motion.needs_animation());
        assert_eq!(motion.intensity, 0.0);
        let settled_rotation = motion.rotation;
        let settled_breath = motion.breath;
        motion.advance(Duration::from_secs(500), false);
        assert_eq!(motion.rotation, settled_rotation);
        assert_eq!(motion.breath, settled_breath);
        assert!(motion.frame().dots.iter().all(|dot| dot.opacity > 0.0));
    }

    #[test]
    fn connecting_and_reversing_a_transition_preserve_the_current_pose() {
        let mut motion = OrbMotion::new();
        assert!(!motion.needs_animation());
        let rotation = motion.rotation;
        motion.advance(Duration::ZERO, true);
        assert!(motion.needs_animation());
        assert_eq!(motion.rotation, rotation);
        assert_eq!(motion.intensity, 0.0);
        motion.advance(Duration::from_millis(300), true);
        assert!((motion.intensity - 0.5).abs() < 1e-12);
        let rotation = motion.rotation;
        let intensity = motion.intensity;
        motion.advance(Duration::ZERO, false);
        assert_eq!(motion.rotation, rotation);
        assert_eq!(motion.intensity, intensity);
        motion.advance(Duration::from_millis(200), false);
        assert!(motion.intensity < intensity);
        let rotation = motion.rotation;
        let intensity = motion.intensity;
        motion.advance(Duration::ZERO, true);
        assert_eq!(motion.rotation, rotation);
        assert_eq!(motion.intensity, intensity);
        motion.advance(Duration::from_millis(600), true);
        assert_eq!(motion.intensity, 1.0);
        assert!(motion.needs_animation());
    }
}
