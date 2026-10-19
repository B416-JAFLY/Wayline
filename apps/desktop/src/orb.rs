use crate::orb_geometry::OrbMotion;
use gpui::*;
use std::time::{Duration, Instant};

const SIDE: f32 = 152.;
const RADIUS: f32 = 57.;

/// The homepage illustration owns its clock; background polling never drives animation.
#[derive(Default)]
pub struct ConnectionOrb {
    motion: OrbMotion,
    last_frame: Option<Instant>,
}

impl ConnectionOrb {
    pub fn update(&mut self, visible: bool, connected: bool) {
        self.update_at(Instant::now(), visible, connected);
    }

    fn update_at(&mut self, now: Instant, visible: bool, connected: bool) {
        if !visible {
            self.last_frame = None;
            return;
        }
        // A hidden/minimized window or suspended computer must not accumulate a
        // large time step and make the sphere jump on its first visible frame.
        let elapsed = self
            .last_frame
            .replace(now)
            .map(|last| now.saturating_duration_since(last))
            .unwrap_or_default()
            .min(Duration::from_millis(50));
        self.motion.advance(elapsed, connected);
    }

    pub fn element(&self) -> impl IntoElement {
        let frame = self.motion.frame();
        let animate = self.last_frame.is_some() && self.motion.needs_animation();
        canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                let center = bounds.center();
                // A soft light behind the points gives them presence without a
                // solid disc or an outline competing with the sphere silhouette.
                let glow_radius = px(RADIUS * 0.62);
                window.paint_shadows(
                    Bounds::new(
                        center - point(glow_radius, glow_radius),
                        size(glow_radius * 2., glow_radius * 2.),
                    ),
                    glow_radius.into(),
                    &[BoxShadow {
                        color: Hsla::from(rgb(0x0a84ff)).opacity(0.09 * frame.intensity),
                        offset: point(px(0.), px(0.)),
                        blur_radius: px(26.),
                        spread_radius: px(3.),
                    }],
                );
                for dot in frame.dots {
                    let depth = (dot.depth + 1.) * 0.5;
                    let diameter = px(dot.diameter * RADIUS);
                    let position = center + point(px(dot.x * RADIUS), px(dot.y * RADIUS));
                    let mut color = Rgba {
                        r: 0.12 + depth * 0.43,
                        g: 0.42 + depth * 0.35,
                        b: 1.,
                        a: dot.opacity,
                    };
                    // Desaturate disconnected dots while retaining their depth.
                    let saturation = 0.25 + frame.intensity * 0.75;
                    color.r = 0.40 + (color.r - 0.40) * saturation;
                    color.g = 0.47 + (color.g - 0.47) * saturation;
                    color.b = 0.57 + (color.b - 0.57) * saturation;
                    window.paint_quad(
                        fill(
                            Bounds::new(
                                position - point(diameter * 0.5, diameter * 0.5),
                                size(diameter, diameter),
                            ),
                            color,
                        )
                        .corner_radii(diameter * 0.5),
                    );
                }
                if animate {
                    window.request_animation_frame();
                }
            },
        )
        .size(px(SIDE))
        .flex_shrink_0()
    }
}

#[cfg(test)]
mod tests {
    use super::{ConnectionOrb, Duration, Instant};

    #[test]
    fn hiding_pauses_the_clock_and_reopening_does_not_jump() {
        let mut orb = ConnectionOrb::default();
        let start = Instant::now();
        orb.update_at(start, true, true);
        orb.update_at(start + Duration::from_millis(16), true, true);
        let before = orb.motion.frame();
        orb.update_at(start + Duration::from_millis(32), false, true);
        assert!(orb.last_frame.is_none());
        orb.update_at(start + Duration::from_secs(60), true, true);
        let after = orb.motion.frame();
        assert_eq!(before.intensity, after.intensity);
        for (before, after) in before.dots.iter().zip(after.dots.iter()) {
            assert_eq!((before.x, before.y), (after.x, after.y));
        }
    }
}
