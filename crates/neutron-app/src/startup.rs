//! Cold-start measurement, broken down by phase.
//!
//! The plan targets <100ms from process start to first painted frame. A single
//! total is not enough to defend that: when the number regresses you need to
//! know *which* phase moved, because the fixes are completely different — GPU
//! device creation is a backend choice, font loading is an asset choice, and
//! first-frame layout is our own code.
//!
//! Phases measured, all relative to process start:
//!
//! * `setup`  — logging and option construction, before the window exists.
//! * `gpu`    — winit window plus wgpu adapter, device, and surface. Normally
//!   the dominant cost, and almost entirely outside our control.
//! * `paint`  — building and rendering the first frame, which *is* our code.

use std::time::{Duration, Instant};

pub struct StartupTimer {
    launched: Instant,
    setup_done: Option<Duration>,
    gpu_ready: Option<Duration>,
    first_frame: Option<Duration>,
    frames: FrameStats,
}

/// Rolling frame-time statistics.
///
/// The steady-state target is <8ms p99 while scrolling. p99 rather than mean
/// because the failure mode that matters is a *stutter* — one 40ms frame in a
/// hundred is plainly visible, and a mean would hide it completely.
///
/// Measures the **cost of building a frame**, not the interval between frames.
/// The first version timed the gap between successive calls, which is a
/// different quantity entirely: Neutron caps its idle repaint rate for the
/// ambient animation, so an interval-based metric reports ~33ms when the app is
/// deliberately idling and would flag healthy throttling as a stall.
pub struct FrameStats {
    /// Ring of recent frame build durations in milliseconds.
    samples: Vec<f32>,
    cursor: usize,
    filled: bool,
    since_report: usize,
}

impl FrameStats {
    /// Two seconds of history at 120fps — long enough for a stable p99, short
    /// enough that a stall shows up while the user is still scrolling.
    const WINDOW: usize = 240;
    /// Report roughly every two seconds of activity.
    const REPORT_EVERY: usize = 240;

    fn new() -> Self {
        Self {
            samples: vec![0.0; Self::WINDOW],
            cursor: 0,
            filled: false,
            since_report: 0,
        }
    }

    fn record(&mut self, build: Duration) {
        self.samples[self.cursor] = build.as_secs_f32() * 1000.0;
        self.cursor = (self.cursor + 1) % Self::WINDOW;
        if self.cursor == 0 {
            self.filled = true;
        }

        self.since_report += 1;
        if self.since_report >= Self::REPORT_EVERY {
            self.since_report = 0;
            if let Some((p50, p99)) = self.percentiles() {
                // Debug level: this is diagnostic, not something a user needs
                // in their log by default.
                tracing::debug!("frame time p50 {p50:.2}ms  p99 {p99:.2}ms");
            }
        }
    }

    /// Median and 99th percentile of the current window, in milliseconds.
    pub fn percentiles(&self) -> Option<(f32, f32)> {
        let n = if self.filled {
            Self::WINDOW
        } else {
            self.cursor
        };
        if n < 8 {
            return None;
        }

        let mut sorted: Vec<f32> = self.samples[..n].to_vec();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let idx = |q: f32| ((n as f32 * q) as usize).min(n - 1);
        Some((sorted[idx(0.50)], sorted[idx(0.99)]))
    }
}

impl StartupTimer {
    pub fn new(launched: Instant) -> Self {
        Self {
            launched,
            setup_done: None,
            gpu_ready: None,
            first_frame: None,
            frames: FrameStats::new(),
        }
    }

    /// Called just before handing control to eframe.
    pub fn mark_setup_done(&mut self) {
        self.setup_done.get_or_insert_with(|| self.launched.elapsed());
    }

    /// Called from the eframe creation callback — the window and GPU device
    /// both exist by this point.
    pub fn mark_gpu_ready(&mut self) {
        self.gpu_ready.get_or_insert_with(|| self.launched.elapsed());
    }

    /// Call once per frame. Records and reports the first painted frame, then
    /// becomes a no-op.
    /// `build` is how long constructing this frame took — measured around the
    /// app's `ui()` body, excluding GPU submission and any vsync wait.
    pub fn mark_frame(&mut self, build: Duration) {
        self.frames.record(build);
        if self.first_frame.is_some() {
            return;
        }
        let total = self.launched.elapsed();
        self.first_frame = Some(total);

        let ms = |d: Option<Duration>| d.map_or(f64::NAN, |d| d.as_secs_f64() * 1000.0);
        let (setup, gpu, total_ms) = (ms(self.setup_done), ms(self.gpu_ready), ms(Some(total)));

        // Phases are cumulative from launch, so report each one's own share to
        // make the dominant cost obvious at a glance.
        let report = format!(
            "cold start {total_ms:.0}ms (setup {setup:.0}ms, gpu +{:.0}ms, paint +{:.0}ms)",
            gpu - setup,
            total_ms - gpu,
        );

        if total_ms > 100.0 {
            tracing::warn!("{report} — over the 100ms budget");
        } else {
            tracing::info!("{report}");
        }
    }

}

impl StartupTimer {
    /// Rolling frame-time percentiles, for the status-bar readout.
    pub fn frame_percentiles(&self) -> Option<(f32, f32)> {
        self.frames.percentiles()
    }
}
