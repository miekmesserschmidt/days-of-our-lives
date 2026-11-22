//! Where things go. Everything is computed in abstract coordinates — `t` along the time
//! axis (0..len) and `c` across it (0..breadth) — and only `Geo::rect` knows whether
//! time runs left→right (horizontal) or top→bottom (vertical).

use crate::ics::Event;
use chrono::{NaiveDate, NaiveDateTime};
use dool_plugin_sdk::Rect;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Geo {
    pub vertical: bool,
    pub len: f64,
    pub breadth: f64,
}

impl Geo {
    pub fn new(vertical: bool, width: f64, height: f64) -> Geo {
        if vertical { Geo { vertical, len: height, breadth: width } } else { Geo { vertical, len: width, breadth: height } }
    }

    /// Position along the time axis of a time of day given in minutes (0..1440).
    pub fn t(&self, minutes: f64) -> f64 {
        minutes.clamp(0.0, 1440.0) / 1440.0 * self.len
    }

    pub fn rect(&self, t0: f64, t1: f64, c0: f64, c1: f64) -> Rect {
        if self.vertical { Rect { x: c0, y: t0, w: c1 - c0, h: t1 - t0 } } else { Rect { x: t0, y: c0, w: t1 - t0, h: c1 - c0 } }
    }
}

/// Minutes since the start of `day`, clamped to the day.
pub fn minutes(dt: NaiveDateTime, day: NaiveDate) -> f64 {
    let start = day.and_hms_opt(0, 0, 0).unwrap();
    ((dt - start).num_seconds() as f64 / 60.0).clamp(0.0, 1440.0)
}

/// Greedy lanes for spans sorted by start: each goes into the first lane that is free.
pub fn assign_lanes(spans: &[(f64, f64)]) -> (Vec<usize>, usize) {
    let mut lane_end: Vec<f64> = Vec::new();
    let lanes = spans
        .iter()
        .map(|&(s, e)| match lane_end.iter().position(|&end| end <= s) {
            Some(i) => {
                lane_end[i] = e;
                i
            }
            None => {
                lane_end.push(e);
                lane_end.len() - 1
            }
        })
        .collect();
    (lanes, lane_end.len().max(1))
}

pub struct Layout {
    pub geo: Geo,
    /// Cross-axis band of the hour labels, if there is room for them.
    pub labels: Option<(f64, f64)>,
    /// Cross-axis band of the events (timed and all-day).
    pub events: (f64, f64),
    /// Rectangle per event, same order as the input.
    pub rects: Vec<Rect>,
    /// Hours between labels.
    pub hour_step: u32,
}

pub fn layout(events: &[Event], day: NaiveDate, geo: Geo, show_all_day: bool, hour_labels: bool, font_size: f64) -> Layout {
    let label_size = font_size.max(6.0) + 2.0;
    let labels = (hour_labels && geo.breadth >= 3.0 * label_size).then_some((geo.breadth - label_size, geo.breadth));
    let events_band = (0.0, labels.map_or(geo.breadth, |l| l.0));
    let has_all_day = show_all_day && events.iter().any(|e| e.all_day);
    let strip = if has_all_day { (events_band.1 * 0.18).clamp(3.0, 8.0) } else { 0.0 };

    let timed: Vec<usize> = (0..events.len()).filter(|&i| !events[i].all_day).collect();
    let spans: Vec<(f64, f64)> = timed.iter().map(|&i| (minutes(events[i].start, day), minutes(events[i].end, day))).collect();
    let (lanes, n_lanes) = assign_lanes(&spans);
    let (c0, c1) = (events_band.0 + strip, events_band.1);
    let lane_w = (c1 - c0) / n_lanes as f64;

    let hidden = geo.rect(0.0, 0.0, 0.0, 0.0);
    let mut rects = vec![hidden; events.len()];
    for (k, &i) in timed.iter().enumerate() {
        let (s, e) = spans[k];
        let (t0, t1) = (geo.t(s), geo.t(e).max(geo.t(s) + 2.0)); // zero-length events stay visible
        let lane = lanes[k] as f64;
        rects[i] = geo.rect(t0, t1, c0 + lane * lane_w, c0 + (lane + 1.0) * lane_w - 0.5);
    }
    if has_all_day {
        for (i, e) in events.iter().enumerate().filter(|(_, e)| e.all_day) {
            rects[i] = geo.rect(geo.t(minutes(e.start, day)), geo.t(minutes(e.end, day)), events_band.0, events_band.0 + strip - 0.5);
        }
    }
    let per_hour = geo.len / 24.0;
    let needed = if geo.vertical { label_size * 1.2 } else { font_size * 1.6 };
    let hour_step = [1, 2, 3, 4, 6, 12].into_iter().find(|s| *s as f64 * per_hour >= needed).unwrap_or(24);
    Layout { geo, labels, events: events_band, rects, hour_step }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lanes_are_reused_when_free() {
        let (lanes, n) = assign_lanes(&[(0.0, 60.0), (30.0, 90.0), (60.0, 120.0), (100.0, 110.0)]);
        assert_eq!((lanes, n), (vec![0, 1, 0, 1], 2));
    }

    #[test]
    fn orientation_only_swaps_axes() {
        let h = Geo::new(false, 1440.0, 30.0);
        let v = Geo::new(true, 30.0, 1440.0);
        assert_eq!(h.rect(h.t(60.0), h.t(120.0), 0.0, 10.0), Rect { x: 60.0, y: 0.0, w: 60.0, h: 10.0 });
        assert_eq!(v.rect(v.t(60.0), v.t(120.0), 0.0, 10.0), Rect { x: 0.0, y: 60.0, w: 10.0, h: 60.0 });
    }
}
