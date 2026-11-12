//! File output for a trace: CSV, a labelled SVG, and a bare PNG.

use std::fmt::Write as _;
use std::path::Path;

use plotters::prelude::*;

use crate::cal::Calibration;
use crate::trace::{fmt_axis, Trace};
use crate::Result;

/// Current against time: `(seconds from the start of the trace, nA)` for each window of `bin_ms`.
pub fn series(trace: &Trace, cal: &Calibration, bin_ms: u64) -> Vec<(f64, f64)> {
    let bin_ms = bin_ms.max(1);
    let (start, end) = (trace.start_ms(), trace.end_ms());
    let n = ((end - start) / bin_ms) as usize;
    trace
        .columns(start as i64, bin_ms, n)
        .into_iter()
        .enumerate()
        .filter_map(|(i, pps)| Some(((i as f64 + 0.5) * bin_ms as f64 / 1000.0, cal.na(pps?))))
        .collect()
}

/// Largest of 1, 2, 5 x 10^k that divides `span` into at most `max_ticks` steps.
pub fn nice_step(span: f64, max_ticks: usize) -> f64 {
    if span.is_nan() || span <= 0.0 {
        return 1.0;
    }
    let raw = span / max_ticks.max(1) as f64;
    let mag = 10f64.powf(raw.log10().floor());
    [1.0, 2.0, 5.0, 10.0]
        .iter()
        .map(|m| m * mag)
        .find(|s| *s >= raw)
        .unwrap_or(10.0 * mag)
}

pub fn write_csv(path: &Path, trace: &Trace, cal: &Calibration, bin_ms: u64) -> Result<()> {
    let mut out = String::from("time_s,current_ua\n");
    for (t, na) in series(trace, cal, bin_ms) {
        let _ = writeln!(out, "{t:.4},{:.3}", na / 1000.0);
    }
    std::fs::write(path, out)?;
    Ok(())
}

/// A self-contained SVG with axes and labels. Needs no fonts at render time on our side.
pub fn write_svg(path: &Path, trace: &Trace, cal: &Calibration, bin_ms: u64, title: &str) -> Result<()> {
    let data = series(trace, cal, bin_ms);
    if data.is_empty() {
        return Err("nothing to plot".into());
    }
    let (w, h, left, right, top, bottom) = (1000.0, 380.0, 78.0, 16.0, 30.0, 40.0);
    let (pw, ph) = (w - left - right, h - top - bottom);
    let t_max = data.last().map_or(1.0, |d| d.0).max(1e-9);
    let y_step = nice_step(data.iter().map(|d| d.1).fold(0.0, f64::max).max(1.0), 5);
    let y_max = (data.iter().map(|d| d.1).fold(0.0, f64::max) / y_step).ceil().max(1.0) * y_step;
    let x = |t: f64| left + t / t_max * pw;
    let y = |na: f64| top + ph - na / y_max * ph;

    let mut s = String::new();
    let _ = writeln!(
        s,
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" font-family="sans-serif" font-size="12">
<rect width="{w}" height="{h}" fill="#ffffff"/>
<text x="{left}" y="18" font-size="13" fill="#222222">{}</text>"##,
        title.replace('&', "&amp;").replace('<', "&lt;")
    );
    let mut v = 0.0;
    while v <= y_max * 1.0001 {
        let _ = writeln!(
            s,
            r##"<line x1="{left}" x2="{:.1}" y1="{1:.1}" y2="{1:.1}" stroke="#dddddd"/>
<text x="{2:.1}" y="{3:.1}" text-anchor="end" fill="#444444">{4}</text>"##,
            left + pw,
            y(v),
            left - 6.0,
            y(v) + 4.0,
            fmt_axis(v)
        );
        v += y_step;
    }
    let t_step = nice_step(t_max, 10);
    let mut t = 0.0;
    while t <= t_max * 1.0001 {
        let _ = writeln!(
            s,
            r##"<line x1="{0:.1}" x2="{0:.1}" y1="{1:.1}" y2="{2:.1}" stroke="#444444"/>
<text x="{0:.1}" y="{3:.1}" text-anchor="middle" fill="#444444">{4} s</text>"##,
            x(t),
            top + ph,
            top + ph + 5.0,
            top + ph + 19.0,
            (t * 1000.0).round() / 1000.0
        );
        t += t_step;
    }
    let pts: Vec<String> = data.iter().map(|d| format!("{:.1},{:.1}", x(d.0), y(d.1))).collect();
    let _ = writeln!(
        s,
        r##"<polyline fill="none" stroke="#1f6feb" stroke-width="1.2" stroke-linejoin="round" points="{}"/>
<rect x="{left}" y="{top}" width="{pw}" height="{ph}" fill="none" stroke="#444444"/>
</svg>"##,
        pts.join(" ")
    );
    std::fs::write(path, s)?;
    Ok(())
}

/// A PNG of the trace without text (drawing text would need a font library).
/// The Y axis runs from zero to the largest value.
pub fn write_png(path: &Path, trace: &Trace, cal: &Calibration, bin_ms: u64) -> Result<()> {
    let data = series(trace, cal, bin_ms);
    if data.is_empty() {
        return Err("nothing to plot".into());
    }
    let t_max = data.last().map_or(1.0, |d| d.0);
    let y_max = data.iter().map(|d| d.1).fold(0.0, f64::max).max(1.0) * 1.05;
    let draw = || -> std::result::Result<(), Box<dyn std::error::Error>> {
        let root = BitMapBackend::new(path, (1400, 480)).into_drawing_area();
        root.fill(&WHITE)?;
        let mut chart = ChartBuilder::on(&root)
            .margin(20)
            .build_cartesian_2d(0f64..t_max, 0f64..y_max)?;
        chart
            .configure_mesh()
            .disable_x_axis()
            .disable_y_axis()
            .disable_mesh()
            .draw()?;
        chart.draw_series(std::iter::once(PathElement::new(
            vec![(0.0, 0.0), (t_max, 0.0)],
            BLACK.mix(0.2),
        )))?;
        chart.draw_series(LineSeries::new(data.iter().copied(), &BLUE))?;
        root.present()?;
        Ok(())
    };
    draw().map_err(|e| e.to_string().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_are_round() {
        assert_eq!(nice_step(10.0, 10), 1.0);
        assert_eq!(nice_step(19.9, 10), 2.0);
        assert_eq!(nice_step(3300.0, 5), 1000.0);
        assert_eq!(nice_step(0.9, 5), 0.2);
    }
}
