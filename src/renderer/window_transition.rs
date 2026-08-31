//! Floating-window open/close transition animations.
//!
//! Adds fade + scale transitions when floating windows appear or are
//! destroyed, with a configurable spring style. All transition state lives on
//! [`WindowTransition`], stored as a field on `RenderedWindow`; the spring
//! math is a pure function of elapsed time so it is trivially testable.
//!
//! Settings (RendererSettings):
//! - `neovide_float_transition_enabled` (bool, default true)
//! - `neovide_float_transition_spring` ("smooth" | "expo" | "bouncy", default "smooth")
//! - `neovide_float_transition_open_length` (seconds, default 0.2)
//! - `neovide_float_transition_close_length` (seconds, default 0.15)
//! - `neovide_float_transition_bounce_strength` (0.0–0.5 overshoot fraction
//!   of the `bouncy` style, default 0.2; 0 disables the overshoot)
//!
//! Only floating windows participate (`anchor_info.is_some()`), never message
//! windows (the cmdline would flicker on every keystroke) and never the root
//! grid. With `--no-multigrid` floats never exist as separate grids, so the
//! feature is inert there as well.

use rmpv::Value;
use skia_safe::{Canvas, Matrix, Paint, Point, Rect, canvas::SaveLayerRec};

use crate::{error_msg, settings::ParseFromValue};

use super::{RenderedWindow, RendererSettings, animation_utils::lerp};
use crate::units::{PixelRect, to_skia_rect};

/// Initial/final scale of the shrink+fade transition.
const TRANSITION_SCALE: f32 = 0.9;
/// Spring styles do not settle exactly at `length`; run 2x the given length.
/// By then the response is within ~3% of the target, which is subpixel for
/// the scale range and invisible for alpha.
const SPRING_SETTLE_FACTOR: f32 = 2.0;
/// Maximum configurable overshoot fraction for the `bouncy` style. Past
/// ~50% the oscillation takes so long to settle that the animation feels
/// broken.
const BOUNCE_STRENGTH_MAX: f32 = 0.5;
/// Damping ratio used when `bounce_strength` is 0 (no overshoot).
const CRITICALLY_DAMPED_ZETA: f32 = 1.0;

// ---------------------------------------------------------------------------
// Spring styles
// ---------------------------------------------------------------------------

/// Progress curve style for the open/close transitions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum SpringStyle {
    /// Critically damped spring, matches the cursor animation feel.
    #[default]
    Smooth,
    /// Matches the window position animation (`ease_out` on open,
    /// `ease_in` on close).
    Expo,
    /// Underdamped spring with a visible overshoot.
    Bouncy,
}

impl ParseFromValue for SpringStyle {
    fn parse_from_value(&mut self, value: Value) {
        match value.as_str() {
            Some("smooth") => *self = Self::Smooth,
            Some("expo") => *self = Self::Expo,
            Some("bouncy") => *self = Self::Bouncy,
            _ => {
                error_msg!(
                    "Setting expected \"smooth\", \"expo\", or \"bouncy\", but received {value:?}"
                );
            }
        }
    }
}

impl From<SpringStyle> for Value {
    fn from(value: SpringStyle) -> Self {
        Value::from(match value {
            SpringStyle::Smooth => "smooth",
            SpringStyle::Expo => "expo",
            SpringStyle::Bouncy => "bouncy",
        })
    }
}

// ---------------------------------------------------------------------------
// Spring progress (pure functions of elapsed time)
// ---------------------------------------------------------------------------

/// Progress (0→1, may overshoot above 1) of the *open* transition.
pub fn open_progress(style: SpringStyle, t: f32, length: f32, bounce_strength: f32) -> f32 {
    if t <= 0.0 {
        return 0.0;
    }
    match style {
        // ease_out_expo (matches the window position animation), but exactly
        // 1.0 at the end so the final frame lands on the finished state.
        SpringStyle::Expo => {
            let x = t / length;
            if x >= 1.0 { 1.0 } else { 1.0 - 2.0f32.powf(-10.0 * x) }
        }
        // Step response of a damped spring from rest at 0 to rest at 1.
        SpringStyle::Smooth | SpringStyle::Bouncy => {
            spring_step_response(spring_zeta(style, bounce_strength), t, length)
        }
    }
}

/// Progress (0→1) of the *close* transition.
pub fn close_progress(style: SpringStyle, t: f32, length: f32, bounce_strength: f32) -> f32 {
    if t <= 0.0 {
        return 0.0;
    }
    match style {
        // ease_in_expo: accelerate away.
        SpringStyle::Expo => {
            let x = t / length;
            if x >= 1.0 { 1.0 } else { 2.0f32.powf(10.0 * x - 10.0) }
        }
        SpringStyle::Smooth | SpringStyle::Bouncy => {
            spring_step_response(spring_zeta(style, bounce_strength), t, length)
        }
    }
}

/// Damping ratio for a spring style. `bounce_strength` is interpreted as the
/// overshoot fraction: overshoot = e^(-πζ/√(1-ζ²)), inverted to
/// ζ = k/√(π²+k²) with k = -ln(strength). So 0 → ζ=1 (no bounce, identical
/// to `smooth`), 0.2 → ζ≈0.456 (~20% overshoot), 0.5 → ζ≈0.215 (~50%).
fn spring_zeta(style: SpringStyle, bounce_strength: f32) -> f32 {
    match style {
        SpringStyle::Smooth => CRITICALLY_DAMPED_ZETA,
        SpringStyle::Expo => CRITICALLY_DAMPED_ZETA,
        SpringStyle::Bouncy => {
            let strength = bounce_strength.clamp(0.0, BOUNCE_STRENGTH_MAX);
            if strength <= 0.0 {
                return CRITICALLY_DAMPED_ZETA;
            }
            let k = -strength.ln();
            k / (std::f32::consts::PI * std::f32::consts::PI + k * k).sqrt()
        }
    }
}

/// Closed-form step response of a damped harmonic oscillator driven from 0
/// to 1, using the same `omega = 4 / length` convention as
/// `CriticallyDampedSpringAnimation` (2% settle time ≈ `length` for zeta=1).
fn spring_step_response(zeta: f32, t: f32, length: f32) -> f32 {
    let omega = 4.0 / length;
    let decay = (-zeta * omega * t).exp();
    if zeta >= 1.0 {
        // Critically damped: 1 - (1 + ωt)·e^(-ωt)
        1.0 - (1.0 + omega * t) * decay
    } else {
        // Underdamped: 1 - e^(-ζωt)·(cos(ω_d t) + (ζω/ω_d)·sin(ω_d t))
        let omega_d = omega * (1.0 - zeta * zeta).sqrt();
        1.0 - decay * ((omega_d * t).cos() + (zeta * omega / omega_d) * (omega_d * t).sin())
    }
}

/// Matrix scaling around `center` by `scale` (affine, no API guessing).
pub(crate) fn scale_about_center_matrix(scale: f32, center: Point) -> Matrix {
    Matrix::new_all(
        scale,
        0.0,
        center.x - scale * center.x,
        0.0,
        scale,
        center.y - scale * center.y,
        0.0,
        0.0,
        1.0,
    )
}

// ---------------------------------------------------------------------------
// Per-window transition state machine
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    Inactive,
    Opening {
        elapsed: f32,
    },
    Closing {
        elapsed: f32,
    },
    /// Close animation finished; window is awaiting removal by the renderer.
    Closed,
}

/// Fade + scale transition state for one window.
#[derive(Clone, Copy, Debug)]
pub struct WindowTransition {
    phase: Phase,
    style: SpringStyle,
    open_length: f32,
    close_length: f32,
    bounce_strength: f32,
}

impl Default for WindowTransition {
    fn default() -> Self {
        Self::new()
    }
}

impl WindowTransition {
    pub fn new() -> Self {
        Self {
            phase: Phase::Inactive,
            style: SpringStyle::default(),
            open_length: 0.2,
            close_length: 0.15,
            bounce_strength: 0.2,
        }
    }

    /// Wall-clock duration of the open animation. Spring styles need longer
    /// than `length` to settle; a bouncier spring oscillates longer, so its
    /// settle factor grows as `1/zeta`.
    fn duration_open(&self) -> f32 {
        self.open_length * self.settle_factor()
    }

    fn duration_close(&self) -> f32 {
        self.close_length * self.settle_factor()
    }

    fn settle_factor(&self) -> f32 {
        match self.style {
            SpringStyle::Expo => 1.0,
            SpringStyle::Smooth => SPRING_SETTLE_FACTOR,
            SpringStyle::Bouncy => {
                let zeta = spring_zeta(self.style, self.bounce_strength);
                SPRING_SETTLE_FACTOR.max(1.0 / zeta)
            }
        }
    }

    /// Begin (or restart) the open transition with the current settings.
    pub fn begin_open(&mut self, settings: &RendererSettings) {
        self.style = settings.float_transition_spring;
        self.open_length = settings.float_transition_open_length.max(1.0 / 120.0);
        self.close_length = settings.float_transition_close_length.max(1.0 / 120.0);
        self.bounce_strength =
            settings.float_transition_bounce_strength.clamp(0.0, BOUNCE_STRENGTH_MAX);
        self.phase = Phase::Opening { elapsed: 0.0 };
    }

    /// Begin (or restart) the close transition with the current settings.
    pub fn begin_close(&mut self, settings: &RendererSettings) {
        self.style = settings.float_transition_spring;
        self.open_length = settings.float_transition_open_length.max(1.0 / 120.0);
        self.close_length = settings.float_transition_close_length.max(1.0 / 120.0);
        self.bounce_strength =
            settings.float_transition_bounce_strength.clamp(0.0, BOUNCE_STRENGTH_MAX);
        self.phase = Phase::Closing { elapsed: 0.0 };
    }

    /// Advance the animation by `dt`. Returns `true` while still animating.
    pub fn update(&mut self, dt: f32) -> bool {
        match self.phase {
            Phase::Opening { elapsed } => {
                let elapsed = elapsed + dt;
                if elapsed >= self.duration_open() {
                    self.phase = Phase::Inactive;
                    false
                } else {
                    self.phase = Phase::Opening { elapsed };
                    true
                }
            }
            Phase::Closing { elapsed } => {
                let elapsed = elapsed + dt;
                if elapsed >= self.duration_close() {
                    self.phase = Phase::Closed;
                    false
                } else {
                    self.phase = Phase::Closing { elapsed };
                    true
                }
            }
            Phase::Inactive | Phase::Closed => false,
        }
    }

    /// Returns `(alpha, scale)` to apply while a transition is running, or
    /// `None` when the window should draw normally.
    pub fn transform(&self) -> Option<(f32, f32)> {
        match self.phase {
            Phase::Opening { elapsed } => {
                let p = open_progress(self.style, elapsed, self.open_length, self.bounce_strength);
                // Clamp alpha (springs overshoot), let scale overshoot freely
                // so `bouncy` pops past full size.
                Some((p.clamp(0.0, 1.0), lerp(TRANSITION_SCALE, 1.0, p)))
            }
            Phase::Closing { elapsed } => {
                let p =
                    close_progress(self.style, elapsed, self.close_length, self.bounce_strength);
                Some((1.0 - p.clamp(0.0, 1.0), lerp(1.0, TRANSITION_SCALE, p.clamp(0.0, 1.0))))
            }
            // Stay invisible until removed, otherwise the finished window
            // would flash back at full opacity for one frame.
            Phase::Closed => Some((0.0, TRANSITION_SCALE)),
            Phase::Inactive => None,
        }
    }

    /// `true` when a close transition has completed and the window should be
    /// removed from the rendered set.
    pub fn close_finished(&self) -> bool {
        self.phase == Phase::Closed
    }

    /// Alpha for the shared layer effects (shadow, backdrop blur): `Some`
    /// while open/close is actually running, `None` when the window is
    /// settled or already finished closing (so it cannot zero out the rest
    /// of the layer).
    pub fn active_alpha(&self) -> Option<f32> {
        match self.phase {
            Phase::Opening { .. } | Phase::Closing { .. } => {
                Some(self.transform().unwrap_or((1.0, 1.0)).0)
            }
            Phase::Inactive | Phase::Closed => None,
        }
    }

    /// `true` while the window's draw output must be transition-transformed,
    /// including the invisible-but-not-yet-removed `Closed` state.
    pub fn is_transitioning(&self) -> bool {
        !matches!(self.phase, Phase::Inactive)
    }
}

// ---------------------------------------------------------------------------
// Integration points (operate on RenderedWindow)
// ---------------------------------------------------------------------------

fn eligible(settings: &RendererSettings, is_floating: bool, is_message: bool) -> bool {
    settings.float_transition_enabled && is_floating && !is_message
}

/// Trigger the open transition when a floating window is positioned for the
/// first time, or when a grid id is reused while the old window was still
/// fading out. The caller supplies the floating/message classification
/// because a brand-new window does not carry its anchor yet.
pub(crate) fn on_position(
    window: &mut RenderedWindow,
    was_valid: bool,
    is_floating: bool,
    is_message: bool,
    settings: &RendererSettings,
) {
    if !eligible(settings, is_floating, is_message) {
        return;
    }
    let fresh =
        !was_valid || matches!(window.transition.phase, Phase::Closing { .. } | Phase::Closed);
    if fresh {
        window.transition.begin_open(settings);
    }
}

/// Trigger the open transition when a floating window becomes visible again
/// (`Show` after `Hide`).
pub(crate) fn on_show(
    window: &mut RenderedWindow,
    was_hidden: bool,
    is_floating: bool,
    is_message: bool,
    settings: &RendererSettings,
) {
    if was_hidden && eligible(settings, is_floating, is_message) {
        window.transition.begin_open(settings);
    }
}

/// Called instead of removing the window when `Close` arrives. Returns `true`
/// if the window should be kept around for the close animation. The caller
/// supplies the floating/message classification (same convention as
/// `on_position`/`on_show`).
pub(crate) fn request_close(
    window: &mut RenderedWindow,
    is_floating: bool,
    is_message: bool,
    settings: &RendererSettings,
) -> bool {
    if !eligible(settings, is_floating, is_message) {
        return false;
    }
    if window.transition.phase == Phase::Closed {
        // Already finished closing; remove now.
        return false;
    }
    window.transition.begin_close(settings);
    true
}

/// Free-function wrapper so `RenderedWindow::animate` only needs one line.
pub(crate) fn update(window: &mut RenderedWindow, dt: f32) -> bool {
    window.transition.update(dt)
}

/// Free-function wrapper: `true` when the window can finally be removed.
pub(crate) fn close_finished(window: &RenderedWindow) -> bool {
    window.transition.close_finished()
}

// ---------------------------------------------------------------------------
// Draw-time guard
// ---------------------------------------------------------------------------

/// RAII guard applying the transition's alpha layer + scale matrix around the
/// window region. Restore happens on drop, after all window content is drawn.
pub(crate) struct TransitionGuard<'a> {
    canvas: Option<&'a Canvas>,
}

impl Drop for TransitionGuard<'_> {
    fn drop(&mut self) {
        if let Some(canvas) = self.canvas {
            canvas.restore();
        }
    }
}

/// Wrap the drawing of one window (its share of the layer background fill and
/// all of its surfaces) in the transition transform. Takes the transform
/// snapshot instead of the window so the caller can keep mutating the window
/// while the guard is alive. Returns an inert guard when not transitioning.
pub(crate) fn apply_guard<'a>(
    canvas: &'a Canvas,
    transform: Option<(f32, f32)>,
    pixel_region: PixelRect<f32>,
) -> TransitionGuard<'a> {
    let Some((alpha, scale)) = transform else {
        return TransitionGuard { canvas: None };
    };

    let rect = to_skia_rect(&pixel_region);
    let center = rect.center();
    // Layer bounds must cover the scaled window (plus a small AA margin).
    let width = rect.width() * scale;
    let height = rect.height() * scale;
    let bounds = Rect::from_xywh(
        center.x - width / 2.0 - 2.0,
        center.y - height / 2.0 - 2.0,
        width + 4.0,
        height + 4.0,
    );

    let mut paint = Paint::default();
    paint.set_anti_alias(false);
    paint.set_alpha_f(alpha.clamp(0.0, 1.0));
    canvas.save_layer(&SaveLayerRec::default().bounds(&bounds).paint(&paint));
    canvas.concat(&scale_about_center_matrix(scale, center));

    TransitionGuard { canvas: Some(canvas) }
}

/// Run `draw` inside an alpha layer. Used for the layer shadow and backdrop
/// blur so they fade together with the transitioning windows. `bounds` should
/// cover everything `draw` may paint.
pub(crate) fn with_alpha_layer(
    canvas: &Canvas,
    alpha: f32,
    bounds: &Rect,
    draw: impl FnOnce(&Canvas),
) {
    if alpha >= 1.0 {
        draw(canvas);
        return;
    }

    let mut paint = Paint::default();
    paint.set_anti_alias(false);
    paint.set_alpha_f(alpha.clamp(0.0, 1.0));
    canvas.save_layer(&SaveLayerRec::default().bounds(bounds).paint(&paint));
    draw(canvas);
    canvas.restore();
}

/// Layer-wide alpha for shadow and backdrop blur: the minimum alpha across
/// the layer's transitioning windows, `1.0` when none are transitioning.
/// Windows in a layer usually share their lifecycle (e.g. Telescope's main +
/// preview floats), so a mixed layer briefly dims the shared shadow too.
pub(crate) fn layer_alpha(alphas: &[Option<f32>]) -> f32 {
    alphas.iter().flatten().copied().fold(1.0, f32::min)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(spring: SpringStyle) -> RendererSettings {
        RendererSettings { float_transition_spring: spring, ..RendererSettings::default() }
    }

    /// Advance in fixed steps, returning whether any step reported animating.
    fn advance(t: &mut WindowTransition, total: f32, step: f32) -> bool {
        let mut animated = false;
        let mut elapsed = 0.0;
        while elapsed < total - 1e-6 {
            animated |= t.update(step.min(total - elapsed));
            elapsed += step;
        }
        animated
    }

    // ----- spring math -----

    #[test]
    fn expo_open_starts_at_zero_ends_exactly_at_one() {
        assert_eq!(open_progress(SpringStyle::Expo, 0.0, 0.2, 0.0), 0.0);
        assert_eq!(open_progress(SpringStyle::Expo, 0.2, 0.2, 0.0), 1.0);
        let mid = open_progress(SpringStyle::Expo, 0.1, 0.2, 0.0);
        assert!(mid > 0.9, "expo-out must be fast early, got {mid}");
    }

    #[test]
    fn expo_close_is_ease_in() {
        assert_eq!(close_progress(SpringStyle::Expo, 0.0, 0.15, 0.0), 0.0);
        assert_eq!(close_progress(SpringStyle::Expo, 0.15, 0.15, 0.0), 1.0);
        let mid = close_progress(SpringStyle::Expo, 0.075, 0.15, 0.0);
        assert!(mid < 0.1, "ease-in must be slow early, got {mid}");
    }

    #[test]
    fn smooth_progress_settles_without_overshoot() {
        let length = 0.2;
        assert_eq!(open_progress(SpringStyle::Smooth, 0.0, length, 0.0), 0.0);
        for i in 0..=400 {
            let t = (i as f32 / 400.0) * SPRING_SETTLE_FACTOR * length;
            let p = open_progress(SpringStyle::Smooth, t, length, 0.0);
            assert!((0.0..=(1.0 + 1e-4)).contains(&p), "overshoot at t={t}: {p}");
        }
        let end = open_progress(SpringStyle::Smooth, SPRING_SETTLE_FACTOR * length, length, 0.0);
        assert!(end > 0.99, "smooth must settle near 1 within 2x length, got {end}");
    }

    #[test]
    fn bouncy_progress_overshoots_then_settles() {
        let length = 0.2;
        assert_eq!(open_progress(SpringStyle::Bouncy, 0.0, length, 0.2), 0.0);
        let mut max = 0.0f32;
        for i in 1..=400 {
            let t = (i as f32 / 400.0) * SPRING_SETTLE_FACTOR * length;
            max = max.max(open_progress(SpringStyle::Bouncy, t, length, 0.2));
        }
        assert!(max > 1.05, "bouncy must overshoot, max={max}");
        let end = open_progress(SpringStyle::Bouncy, SPRING_SETTLE_FACTOR * length, length, 0.2);
        assert!((end - 1.0).abs() < 0.05, "bouncy must settle near 1, end={end}");
    }

    #[test]
    fn bouncy_scale_overshoots_but_alpha_stays_in_range() {
        let mut t = WindowTransition::new();
        t.begin_open(&settings(SpringStyle::Bouncy));
        let mut max_scale = 0.0f32;
        let mut max_alpha = 0.0f32;
        for _ in 0..40 {
            t.update(1.0 / 60.0);
            if let Some((alpha, scale)) = t.transform() {
                max_alpha = max_alpha.max(alpha);
                max_scale = max_scale.max(scale);
            }
        }
        assert!(max_alpha <= 1.0, "alpha must never exceed 1, got {max_alpha}");
        assert!(max_scale > 1.005, "scale should overshoot 1.0, got {max_scale}");
    }

    fn settings_with(spring: SpringStyle, strength: f32) -> RendererSettings {
        RendererSettings {
            float_transition_spring: spring,
            float_transition_bounce_strength: strength,
            ..RendererSettings::default()
        }
    }

    #[test]
    fn bounce_strength_maps_to_matching_overshoot() {
        let peak = |strength: f32| -> f32 {
            let length = 0.2;
            let mut max = 0.0f32;
            for i in 1..=2000 {
                let t = (i as f32 / 2000.0) * 8.0 * length;
                max = max.max(open_progress(SpringStyle::Bouncy, t, length, strength));
            }
            max
        };
        let weak = peak(0.2);
        assert!((weak - 1.2).abs() < 0.02, "strength 0.2 ≈ 20% overshoot, got {weak}");
        let strong = peak(0.5);
        assert!((strong - 1.5).abs() < 0.02, "strength 0.5 ≈ 50% overshoot, got {strong}");
        assert!(strong > weak);
    }

    #[test]
    fn zero_bounce_strength_is_critically_damped() {
        let length = 0.2;
        let mut max = 0.0f32;
        for i in 1..=800 {
            let t = (i as f32 / 800.0) * 2.0 * length;
            max = max.max(open_progress(SpringStyle::Bouncy, t, length, 0.0));
        }
        assert!(max <= 1.0 + 1e-4, "strength 0 must not overshoot, got {max}");
    }

    #[test]
    fn bounce_strength_extends_settle_time() {
        let duration = |strength: f32| -> f32 {
            let mut t = WindowTransition::new();
            t.begin_open(&settings_with(SpringStyle::Bouncy, strength));
            let mut elapsed = 0.0;
            while t.update(1.0 / 240.0) {
                elapsed += 1.0 / 240.0;
                if elapsed > 30.0 {
                    break;
                }
            }
            elapsed
        };
        let weak = duration(0.2);
        let strong = duration(0.5);
        assert!(strong > weak * 1.5, "stronger bounce must settle later: {strong} vs {weak}");
    }

    // ----- transition lifecycle -----

    #[test]
    fn open_transition_animates_then_completes() {
        let mut t = WindowTransition::new();
        assert!(t.transform().is_none(), "inactive window must not transform");

        t.begin_open(&settings(SpringStyle::Expo));
        let (alpha0, scale0) = t.transform().unwrap();
        assert!(alpha0 < 0.02, "open starts transparent, got {alpha0}");
        assert!((scale0 - TRANSITION_SCALE).abs() < 1e-4);

        assert!(advance(&mut t, 0.19, 1.0 / 60.0), "must animate near the end");
        assert!(!t.update(0.02), "must finish at duration");
        assert!(t.transform().is_none(), "completed open draws normally");
        assert!(!t.close_finished());
    }

    #[test]
    fn open_alpha_increases_monotonically() {
        let mut t = WindowTransition::new();
        t.begin_open(&settings(SpringStyle::Smooth));
        let mut prev = 0.0;
        for _ in 0..12 {
            t.update(1.0 / 60.0);
            let (alpha, _) = t.transform().unwrap();
            assert!(alpha >= prev, "alpha must not go backwards");
            prev = alpha;
        }
        assert!(prev >= 0.9, "alpha should be nearly complete, got {prev}");
    }

    #[test]
    fn active_alpha_tracks_only_running_phases() {
        let mut t = WindowTransition::new();
        assert_eq!(t.active_alpha(), None, "inactive contributes no alpha");

        t.begin_open(&settings(SpringStyle::Expo));
        assert!(t.active_alpha().is_some(), "opening contributes alpha");

        t.update(1.0); // finish open
        assert_eq!(t.active_alpha(), None, "settled window is fully opaque");

        t.begin_close(&settings(SpringStyle::Expo));
        assert!(t.active_alpha().is_some(), "closing contributes alpha");

        t.update(1.0); // finish close
        assert_eq!(t.active_alpha(), None, "closed window must not zero the layer");
    }

    #[test]
    fn is_transitioning_covers_all_visible_transition_phases() {
        let mut t = WindowTransition::new();
        assert!(!t.is_transitioning());

        t.begin_open(&settings(SpringStyle::Expo));
        assert!(t.is_transitioning());

        t.update(1.0);
        assert!(!t.is_transitioning(), "settled window draws normally");

        t.begin_close(&settings(SpringStyle::Expo));
        assert!(t.is_transitioning());

        t.update(1.0);
        assert!(t.is_transitioning(), "closed window stays excluded until removed");
    }

    #[test]
    fn layer_alpha_is_minimum_over_active_windows() {
        assert_eq!(layer_alpha(&[]), 1.0);
        assert_eq!(layer_alpha(&[None, None]), 1.0, "steady windows keep full alpha");
        assert_eq!(layer_alpha(&[Some(1.0), Some(0.4), None]), 0.4);
        assert_eq!(layer_alpha(&[Some(0.0), Some(0.4)]), 0.0);
    }

    #[test]
    fn close_transition_fades_out_then_marks_finished() {
        let mut t = WindowTransition::new();
        t.begin_open(&settings(SpringStyle::Expo));
        t.update(1.0); // finish open immediately

        t.begin_close(&settings(SpringStyle::Expo));
        let (alpha_start, _) = t.transform().unwrap();
        assert!((alpha_start - 1.0).abs() < 0.02);

        advance(&mut t, 0.14, 0.01);
        assert!(t.transform().unwrap().0 < alpha_start, "alpha must decrease");
        assert!(!t.update(0.02), "close finishes at duration");
        assert!(t.close_finished());

        // Finished close must stay invisible until the renderer removes the
        // window, otherwise it flashes back at full opacity for one frame.
        let (alpha_end, _) = t.transform().unwrap();
        assert!(alpha_end < 1e-4, "finished close must stay invisible, got {alpha_end}");
    }

    #[test]
    fn reopen_clears_pending_close() {
        let mut t = WindowTransition::new();
        t.begin_open(&settings(SpringStyle::Expo));
        t.begin_close(&settings(SpringStyle::Expo));
        assert!(t.transform().is_some());

        t.begin_open(&settings(SpringStyle::Expo));
        assert!(!t.close_finished(), "reopening must clear the close state");
        let (alpha, _) = t.transform().unwrap();
        assert!(alpha < 0.02, "reopen restarts from transparent, got {alpha}");
    }

    // ----- eligibility -----

    #[test]
    fn root_windows_never_transition() {
        let mut w = RenderedWindow::new(1);
        on_position(&mut w, false, false, false, &RendererSettings::default());
        assert!(w.transition.transform().is_none());
        assert!(!request_close(&mut w, false, false, &RendererSettings::default()));
    }

    #[test]
    fn floating_position_opens_transition() {
        let mut w = RenderedWindow::new(7);
        on_position(&mut w, false, true, false, &RendererSettings::default());
        assert!(w.transition.transform().is_some(), "new floating window opens");
        // Close on an eligible floating window must keep it for the animation.
        assert!(request_close(&mut w, true, false, &RendererSettings::default()));
        assert!(w.transition.transform().is_some(), "closing window still transforms");
    }

    #[test]
    fn disabled_settings_skip_transitions() {
        let mut w = RenderedWindow::new(42);
        let s = RendererSettings { float_transition_enabled: false, ..Default::default() };
        on_position(&mut w, false, true, false, &s);
        assert!(w.transition.transform().is_none());
        assert!(!request_close(&mut w, true, false, &s));
    }

    // ----- settings parsing -----

    #[test]
    fn spring_style_parses_from_value() {
        let mut s = SpringStyle::default();
        s.parse_from_value(Value::from("bouncy"));
        assert_eq!(s, SpringStyle::Bouncy);
        s.parse_from_value(Value::from("expo"));
        assert_eq!(s, SpringStyle::Expo);
        s.parse_from_value(Value::from("smooth"));
        assert_eq!(s, SpringStyle::Smooth);
    }

    // ----- draw geometry -----

    #[test]
    fn scale_about_center_maps_center_to_center() {
        let m = scale_about_center_matrix(0.5, Point::new(100.0, 80.0));
        let c = m.map_point(Point::new(100.0, 80.0));
        assert!((c.x - 100.0).abs() < 1e-3 && (c.y - 80.0).abs() < 1e-3);

        // 20px right of the center scales down to 10px.
        let p = m.map_point(Point::new(120.0, 80.0));
        assert!((p.x - 110.0).abs() < 1e-3, "got {}", p.x);
    }
}
