//! Tempo Map - piecewise-linear BPM over time
//!
//! Provides conversions between time (seconds) and beats under variable tempo.

use std::sync::atomic::{AtomicU64, Ordering};

static TEMPO_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

fn next_tempo_id() -> u64 {
    TEMPO_ID_COUNTER.fetch_add(1, Ordering::SeqCst)
}

/// Clamp to positive, finite value. Returns 1.0 for invalid inputs.
fn clamp_pos(x: f64) -> f64 {
    if x.is_finite() && x > 0.0 {
        x
    } else {
        1.0
    }
}

/// A single tempo segment with linear BPM interpolation.
#[derive(Clone, Debug)]
struct TempoSegment {
    t0: f64,     // start time (sec)
    t1: f64,     // end time (sec) - can be infinity
    bpm0: f64,   // bpm at t0
    bpm1: f64,   // bpm at t1
    beats0: f64, // cumulative beats at t0
    beats1: f64, // cumulative beats at t1 - can be infinity
}

/// A tempo map that tracks BPM over time with piecewise-linear segments.
#[derive(Clone, Debug)]
pub struct TempoMap {
    pub id: u64,
    pub version: u64,
    segs: Vec<TempoSegment>,
}

impl TempoMap {
    /// Create a new tempo map with the given initial BPM.
    pub fn new(initial_bpm: f64) -> Self {
        let bpm = clamp_pos(initial_bpm);
        Self {
            id: next_tempo_id(),
            version: 0,
            segs: vec![TempoSegment {
                t0: 0.0,
                t1: f64::INFINITY,
                bpm0: bpm,
                bpm1: bpm,
                beats0: 0.0,
                beats1: f64::INFINITY,
            }],
        }
    }

    /// Clone this tempo map with a new unique ID.
    pub fn clone_with_new_id(&self) -> Self {
        let mut cloned = self.clone();
        cloned.id = next_tempo_id();
        cloned
    }

    /// Get the BPM at a given time.
    pub fn bpm_at_time(&self, t: f64) -> f64 {
        let s = match self.segment_at_time(t) {
            Some(s) => s,
            None => return self.segs.last().unwrap().bpm1,
        };

        if !s.t1.is_finite() || s.t1 == s.t0 {
            return s.bpm0;
        }

        let a = (t - s.t0) / (s.t1 - s.t0);
        s.bpm0 + (s.bpm1 - s.bpm0) * a
    }

    /// Get the cumulative beats at a given time.
    pub fn beats_at_time(&self, t: f64) -> f64 {
        let s = match self.segment_at_time(t) {
            Some(s) => s,
            None => return 0.0,
        };

        let dt = (t - s.t0).max(0.0);
        let l = s.t1 - s.t0;
        let k = if l.is_finite() && l > 0.0 {
            (s.bpm1 - s.bpm0) / l
        } else {
            0.0
        };

        // Integral of BPM: beats = (bpm0 * dt + 0.5 * k * dt^2) / 60
        s.beats0 + (s.bpm0 * dt + 0.5 * k * dt * dt) / 60.0
    }

    /// Get the time at a given cumulative beat count.
    pub fn time_at_beats(&self, target_beats: f64) -> f64 {
        if !target_beats.is_finite() {
            return f64::INFINITY;
        }
        if target_beats <= 0.0 {
            return 0.0;
        }

        // Binary search for the segment containing target_beats
        let mut lo = 0;
        let mut hi = self.segs.len() - 1;
        while lo < hi {
            let mid = (lo + hi) / 2;
            let b1 = self.segs[mid].beats1;
            if b1 >= target_beats {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }

        let s = &self.segs[lo];
        let beats_delta = target_beats - s.beats0;
        if beats_delta <= 0.0 {
            return s.t0;
        }

        let l = s.t1 - s.t0;
        let k = if l.is_finite() && l > 0.0 {
            (s.bpm1 - s.bpm0) / l
        } else {
            0.0
        };

        if k.abs() < 1e-12 {
            // Constant BPM
            let bpm = clamp_pos(s.bpm0);
            return s.t0 + (beats_delta * 60.0) / bpm;
        }

        // Solve: 0.5 * k * dt^2 + bpm0 * dt - beats_delta * 60 = 0
        let bpm0 = s.bpm0;
        let c = beats_delta * 60.0;
        let disc = bpm0 * bpm0 + 2.0 * k * c;
        let sqrt_disc = disc.max(0.0).sqrt();
        let dt = (-bpm0 + sqrt_disc) / k;
        s.t0 + dt.max(0.0)
    }

    /// Set BPM at a specific time. Truncates future segments.
    pub fn set_bpm_at_time(&mut self, bpm: f64, t: f64) {
        let new_bpm = clamp_pos(bpm);
        let time = t.max(0.0);

        let (bpm_at_t, beats_at_t, seg_index) = self.split_at(time);

        // Truncate everything after time
        self.segs.truncate(seg_index + 1);
        let s = self.segs.last_mut().unwrap();
        s.t1 = time;
        s.bpm1 = bpm_at_t;
        s.beats1 = beats_at_t;

        // Add new constant segment
        self.segs.push(TempoSegment {
            t0: time,
            t1: f64::INFINITY,
            bpm0: new_bpm,
            bpm1: new_bpm,
            beats0: beats_at_t,
            beats1: f64::INFINITY,
        });

        self.version += 1;
    }

    /// Ramp to a target BPM over a duration starting at time t.
    pub fn ramp_to_bpm_at_time(&mut self, target_bpm: f64, dur_sec: f64, t: f64) {
        let end_bpm = clamp_pos(target_bpm);
        let time = t.max(0.0);
        let dur = dur_sec.max(0.0);

        if dur <= 0.0 {
            self.set_bpm_at_time(end_bpm, time);
            return;
        }

        let (bpm_at_t, beats_at_t, seg_index) = self.split_at(time);

        // Truncate
        self.segs.truncate(seg_index + 1);
        let s = self.segs.last_mut().unwrap();
        s.t1 = time;
        s.bpm1 = bpm_at_t;
        s.beats1 = beats_at_t;

        // Ramp segment
        let t1 = time + dur;
        let k = (end_bpm - bpm_at_t) / dur;
        let beats1 = beats_at_t + (bpm_at_t * dur + 0.5 * k * dur * dur) / 60.0;

        self.segs.push(TempoSegment {
            t0: time,
            t1,
            bpm0: bpm_at_t,
            bpm1: end_bpm,
            beats0: beats_at_t,
            beats1,
        });

        // Constant after ramp
        self.segs.push(TempoSegment {
            t0: t1,
            t1: f64::INFINITY,
            bpm0: end_bpm,
            bpm1: end_bpm,
            beats0: beats1,
            beats1: f64::INFINITY,
        });

        self.version += 1;
    }

    /// Find the segment containing time t.
    fn segment_at_time(&self, t: f64) -> Option<&TempoSegment> {
        if t <= 0.0 {
            return self.segs.first();
        }

        // Binary search for last segment with t0 <= t
        let mut lo = 0;
        let mut hi = self.segs.len() - 1;
        while lo < hi {
            let mid = (lo + hi + 1) / 2;
            if self.segs[mid].t0 <= t {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }

        self.segs.get(lo)
    }

    /// Split at time t, returning (bpm_at_t, beats_at_t, segment_index).
    fn split_at(&self, t: f64) -> (f64, f64, usize) {
        let time = t.max(0.0);
        let s = self.segment_at_time(time).unwrap();
        let seg_index = self.segs.iter().position(|seg| std::ptr::eq(seg, s)).unwrap();

        let beats_at_t = self.beats_at_time(time);
        let bpm_at_t = self.bpm_at_time(time);

        (bpm_at_t, beats_at_t, seg_index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_constant_tempo() {
        let tm = TempoMap::new(120.0); // 2 beats per second

        assert!((tm.bpm_at_time(0.0) - 120.0).abs() < 1e-10);
        assert!((tm.bpm_at_time(1.0) - 120.0).abs() < 1e-10);

        // At 120 BPM, 1 second = 2 beats
        assert!((tm.beats_at_time(1.0) - 2.0).abs() < 1e-10);
        assert!((tm.beats_at_time(0.5) - 1.0).abs() < 1e-10);

        // Inverse: 4 beats should be at t=2
        assert!((tm.time_at_beats(4.0) - 2.0).abs() < 1e-10);
    }

    #[test]
    fn test_tempo_change() {
        let mut tm = TempoMap::new(60.0); // 1 beat per second

        // At t=2, we have 2 beats
        assert!((tm.beats_at_time(2.0) - 2.0).abs() < 1e-10);

        // Change to 120 BPM at t=2
        tm.set_bpm_at_time(120.0, 2.0);

        // At t=3 (1 sec after change at 120 BPM), we have 2 + 2 = 4 beats
        assert!((tm.beats_at_time(3.0) - 4.0).abs() < 1e-10);
    }

    #[test]
    fn test_time_at_beats_with_tempo_change() {
        let mut tm = TempoMap::new(60.0); // 1 beat/sec

        // Change to 120 BPM at t=1 (at 1 beat)
        tm.set_bpm_at_time(120.0, 1.0);

        // 3 beats: 1 beat in first second, 2 more beats in 1 second = t=2
        let t = tm.time_at_beats(3.0);
        assert!((t - 2.0).abs() < 1e-10);
    }
}
