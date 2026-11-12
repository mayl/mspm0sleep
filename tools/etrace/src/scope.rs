//! The terminal oscilloscope: a scrolling current trace with pause, pan and zoom.

use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{self, Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Axis, Block, BorderType, Chart, Clear, Dataset, GraphType, Paragraph};
use ratatui::{DefaultTerminal, Frame, Terminal};

use crate::cal::{Calibration, SUPPLY_V};
use crate::decode::Decoder;
use crate::plot::nice_step;
use crate::trace::{fmt_axis, fmt_current, Trace};
use crate::Result;

/// What a data source sends to the scope.
pub enum Feed {
    /// Raw stream bytes.
    Data(Vec<u8>),
    /// The source finished, with the reason if it failed.
    End(Result<()>),
}

/// Selectable time spans, in milliseconds.
const SPANS_MS: &[u64] = &[
    100, 200, 500, 1_000, 2_000, 5_000, 10_000, 20_000, 30_000, 60_000, 120_000, 300_000, 600_000, 1_800_000,
    3_600_000, 7_200_000,
];

/// Selectable averaging windows, in points.
const AVERAGES: &[u64] = &[1, 4, 16, 64];

/// Lowest value on the logarithmic axis, in nA.
const LOG_FLOOR_NA: f64 = 100.0;
/// Smallest full-scale value, in nA.
const MIN_FULL_SCALE_NA: f64 = 1_000.0;
/// Width reserved for the Y-axis labels, in cells.
const Y_LABEL_WIDTH: usize = 9;

const TRACE_COLOR: Color = Color::Green;
const MEAN_COLOR: Color = Color::Yellow;

/// Style for labels, axes and borders.
fn dim() -> Style {
    Style::new().fg(Color::DarkGray)
}

/// A rounded, titled frame.
fn panel(title: &str) -> Block<'_> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(dim())
        .title(Line::from(format!(" {title} ")).style(Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)))
}

/// What the chart is showing, for the readouts around it.
struct View {
    /// The part of the time window that holds data, in ms.
    start_ms: u64,
    end_ms: u64,
    /// Time per plotted point, in ms.
    width_ms: u64,
    /// Mean current over the window, in nA.
    mean: Option<f64>,
    /// Lowest and highest plotted point, in nA.
    low: f64,
    peak: f64,
}

/// The key reference, drawn over the middle of the screen.
fn render_help(frame: &mut Frame, area: Rect) {
    const KEYS: &[(&str, &str)] = &[
        ("space", "pause and resume; data keeps arriving while paused"),
        ("+  -", "shorter or longer time span"),
        (
            "\u{2190}  \u{2192}",
            "pan (Shift: by a whole span); panning to the newest data resumes",
        ),
        ("Home  End", "oldest and newest data"),
        ("\u{2191}  \u{2193}", "zoom the current axis in and out"),
        ("a", "auto-range the current axis"),
        ("g", "logarithmic current axis"),
        ("s", "smoothing: average each point over 4, 16 or 64 points"),
        ("f", "fit the whole trace"),
        ("r", "restart the totals"),
        ("q", "quit"),
    ];
    const NOTES: &[&str] = &[
        "The strip under the trace shows everything recorded; the part on",
        "screen is highlighted. The yellow line is the mean of the window.",
        "pps = charge pulses per second, what the probe actually counts.",
    ];
    let (w, h) = (
        74.min(area.width),
        ((KEYS.len() + NOTES.len()) as u16 + 5).min(area.height),
    );
    let rect = Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    };
    let mut lines = vec![Line::default()];
    for (key, what) in KEYS {
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {key:<11}"),
                Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::raw(*what),
        ]));
    }
    lines.push(Line::default());
    lines.extend(
        NOTES
            .iter()
            .map(|note| Line::from(Span::styled(format!("  {note}"), dim()))),
    );
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines)
            .block(panel("Keys").title_bottom(Line::from(" any key closes ").style(dim()).right_aligned())),
        rect,
    );
}

#[derive(Clone, Copy, PartialEq)]
enum YScale {
    Auto,
    /// Full scale in nA.
    Fixed(f64),
}

pub struct Scope {
    trace: Trace,
    decoder: Decoder,
    cal: Calibration,
    /// Left part of the header: where the data comes from.
    source: String,
    /// Data is still arriving.
    live: bool,
    span_ms: u64,
    /// Right edge of the view; None follows the newest data.
    view_end: Option<u64>,
    y: YScale,
    /// Full scale chosen by auto-ranging, in nA.
    auto_full_scale: f64,
    log: bool,
    /// Each point shows the mean over this many points' worth of time, centred on it.
    average: u64,
    /// Totals in the footer count from here.
    totals_from_ms: u64,
    /// One-line message shown in the header (errors, end of stream).
    message: Option<String>,
    /// The key reference is showing.
    help: bool,
    /// Where the calibration came from, shown under its values.
    cal_note: String,
}

impl Scope {
    /// A scope over data still to come.
    pub fn live(rate: u32, cal: Calibration, source: String) -> Scope {
        Scope {
            // Two hours of history at 8 bytes per millisecond is about 58 MB.
            trace: Trace::new(rate).with_history_limit(2 * 3600 * 1000),
            decoder: Decoder::new(),
            cal,
            source,
            live: true,
            span_ms: 10_000,
            view_end: None,
            y: YScale::Auto,
            auto_full_scale: MIN_FULL_SCALE_NA,
            log: false,
            average: 1,
            totals_from_ms: 0,
            message: None,
            help: false,
            cal_note: String::new(),
        }
    }

    /// A scope over a finished capture, showing all of it.
    pub fn file(data: &[u8], default_rate: u32, cal: Calibration, source: String) -> Scope {
        let mut scope = Scope::live(default_rate, cal, source);
        scope.trace = Trace::new(default_rate);
        scope.feed(data);
        scope.live = false;
        scope.fit();
        scope
    }

    /// Add raw stream bytes.
    pub fn feed(&mut self, data: &[u8]) {
        let trace = &mut self.trace;
        self.decoder.feed(data, |e| trace.push(e));
    }

    pub fn trace(&self) -> &Trace {
        &self.trace
    }

    pub fn decoder(&self) -> &Decoder {
        &self.decoder
    }

    /// Show the whole trace.
    fn fit(&mut self) {
        self.span_ms = (self.trace.end_ms() - self.trace.start_ms()).max(SPANS_MS[0]);
        self.view_end = if self.live { None } else { Some(self.trace.end_ms()) };
    }

    /// Set the time span shown, in seconds.
    pub fn set_span(&mut self, seconds: f64) {
        self.span_ms = ((seconds * 1000.0) as u64).max(1);
    }

    /// Put the right edge of the view at `seconds` from the start of the trace.
    pub fn set_end(&mut self, seconds: f64) {
        self.view_end = Some(((seconds * 1000.0) as u64).min(self.trace.end_ms()));
    }

    /// Say where the calibration came from.
    pub fn set_cal_note(&mut self, note: impl Into<String>) {
        self.cal_note = note.into();
    }

    pub fn set_log(&mut self, log: bool) {
        self.log = log;
    }

    fn end(&self) -> u64 {
        self.view_end.unwrap_or(self.trace.end_ms())
    }

    /// The view for `points` horizontal points: (start in ms, width of a point in ms, number of points).
    /// The start is a multiple of the width so the trace does not shimmer as it scrolls.
    fn window(&self, points: usize) -> (i64, u64, usize) {
        let width = self.span_ms.div_ceil(points.max(1) as u64).max(1);
        let n = (self.span_ms / width) as usize;
        let end = self.end() / width * width;
        (end as i64 - (n as u64 * width) as i64, width, n)
    }

    /// Step to the next shorter (`wider == false`) or longer span in the list.
    fn zoom(&mut self, wider: bool) {
        let next = if wider {
            SPANS_MS.iter().find(|s| **s > self.span_ms)
        } else {
            SPANS_MS.iter().rev().find(|s| **s < self.span_ms)
        };
        if let Some(span) = next {
            self.span_ms = *span;
        }
    }

    /// Move the view by `fraction` of a span (negative = back in time).
    fn pan(&mut self, fraction: f64) {
        let step = (self.span_ms as f64 * fraction) as i64;
        let newest = self.trace.end_ms();
        let oldest = (self.trace.start_ms() + self.span_ms / 10).min(newest);
        let end = (self.end() as i64 + step).clamp(oldest as i64, newest.max(oldest) as i64) as u64;
        // Reaching the newest data while it is still arriving resumes following it.
        self.view_end = if self.live && end >= newest { None } else { Some(end) };
    }

    fn scale_y(&mut self, factor_up: bool) {
        let current = match self.y {
            YScale::Auto => self.auto_full_scale,
            YScale::Fixed(v) => v,
        };
        // Step through 1, 2, 5 x 10^k.
        let mag = 10f64.powf(current.log10().floor());
        let lead = (current / mag).round();
        let next = match (factor_up, lead as u32) {
            (true, 1) => 2.0 * mag,
            (true, 2) => 5.0 * mag,
            (true, _) => 10.0 * mag,
            (false, 1) => 0.5 * mag,
            (false, 2) => 1.0 * mag,
            (false, _) => 2.0 * mag,
        };
        self.y = YScale::Fixed(next.clamp(MIN_FULL_SCALE_NA, 1e9));
    }

    /// Handle a key. Returns false when the scope should close.
    pub fn key(&mut self, key: KeyEvent) -> bool {
        if self.help {
            self.help = false;
            return true;
        }
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Char('?') | KeyCode::Char('h') => self.help = true,
            KeyCode::Char('q') | KeyCode::Esc => return false,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return false,
            KeyCode::Char(' ') if self.live => {
                self.view_end = match self.view_end {
                    Some(_) => None,
                    None => Some(self.trace.end_ms()),
                }
            }
            KeyCode::Left => self.pan(if shift { -1.0 } else { -0.1 }),
            KeyCode::Right => self.pan(if shift { 1.0 } else { 0.1 }),
            KeyCode::Home => self.view_end = Some((self.trace.start_ms() + self.span_ms).min(self.trace.end_ms())),
            KeyCode::End => self.view_end = if self.live { None } else { Some(self.trace.end_ms()) },
            KeyCode::Char('+') | KeyCode::Char('=') => self.zoom(false),
            KeyCode::Char('-') | KeyCode::Char('_') => self.zoom(true),
            KeyCode::Up => self.scale_y(false),
            KeyCode::Down => self.scale_y(true),
            KeyCode::Char('a') => self.y = YScale::Auto,
            KeyCode::Char('g') => self.log = !self.log,
            KeyCode::Char('s') => {
                let i = AVERAGES.iter().position(|a| *a == self.average).unwrap_or(0);
                self.average = AVERAGES[(i + 1) % AVERAGES.len()];
            }
            KeyCode::Char('f') => self.fit(),
            KeyCode::Char('r') => self.totals_from_ms = self.trace.end_ms(),
            _ => {}
        }
        true
    }

    pub fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();
        // The stat cards need room; on a short terminal they collapse to one line.
        let cards_height = if area.height >= 18 { 5 } else { 1 };
        let [head, body, strip, cards, keys] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(5),
            Constraint::Length(1),
            Constraint::Length(cards_height),
            Constraint::Length(1),
        ])
        .areas(area);

        let view = self.render_chart(frame, body);
        self.render_header(frame, head);
        self.render_overview(frame, strip, &view);
        self.render_cards(frame, cards, &view);
        self.render_keys(frame, keys);
        if self.help {
            render_help(frame, area);
        }
    }

    /// Draw the trace and return what is on screen.
    fn render_chart(&mut self, frame: &mut Frame, area: Rect) -> View {
        // Inside the border: Y labels, the axis line, then two braille dots per cell.
        let points = (area.width as usize).saturating_sub(2 + Y_LABEL_WIDTH + 1).max(1) * 2;
        let (start, width, n) = self.window(points);
        // With averaging, a point's window extends half the averaging length to each side of it.
        let reach = (self.average - 1) * width / 2;
        let data: Vec<(f64, f64)> = self
            .trace
            .columns(start, width, n)
            .into_iter()
            .enumerate()
            .filter_map(|(i, pps)| {
                let a = start + (i as u64 * width) as i64;
                let pps = match reach {
                    0 => pps?,
                    _ => pps.and(
                        self.trace
                            .pps((a - reach as i64).max(0) as u64, a as u64 + width + reach),
                    )?,
                };
                Some(((a as f64 + 0.5 * width as f64) / 1000.0, self.cal.na(pps)))
            })
            .collect();
        let peak = data.iter().map(|d| d.1).fold(0.0, f64::max);
        let low = data.iter().map(|d| d.1).fold(f64::INFINITY, f64::min);

        let (w_start, w_end) = (
            start.max(self.trace.start_ms() as i64) as u64,
            self.end().min(self.trace.end_ms()),
        );
        let mean = self.trace.pps(w_start, w_end).map(|pps| self.cal.na(pps));

        // Auto-range: grow at once, shrink only when the trace uses under a fifth of the scale.
        let wanted = full_scale_for(peak);
        if wanted > self.auto_full_scale || wanted < self.auto_full_scale / 5.0 {
            self.auto_full_scale = wanted;
        }
        let full_scale = match self.y {
            YScale::Auto => self.auto_full_scale,
            YScale::Fixed(v) => v,
        };

        let log_top = full_scale.log10().ceil().max(LOG_FLOOR_NA.log10() + 1.0);
        let (y_bounds, y_labels): ([f64; 2], Vec<String>) = if self.log {
            let bottom = LOG_FLOOR_NA.log10();
            (
                [bottom, log_top],
                (bottom as i32..=log_top as i32)
                    .map(|d| fmt_axis(10f64.powi(d)))
                    .collect(),
            )
        } else {
            (
                [0.0, full_scale],
                (0..=4).map(|i| fmt_axis(full_scale * i as f64 / 4.0)).collect(),
            )
        };
        // Position on the Y axis for a current in nA.
        let y = |na: f64| match self.log {
            true => na.max(LOG_FLOOR_NA).log10().min(log_top),
            false => na.min(full_scale),
        };
        let plotted: Vec<(f64, f64)> = data.iter().map(|&(t, na)| (t, y(na))).collect();

        let (t0, t1) = (
            start as f64 / 1000.0,
            (start as f64 + (n as u64 * width) as f64) / 1000.0,
        );
        let x_labels: Vec<String> = [t0, (t0 + t1) / 2.0, t1]
            .iter()
            .map(|t| fmt_time(*t, self.span_ms))
            .collect();
        let mean_line: Vec<(f64, f64)> = mean.map(|m| vec![(t0, y(m)), (t1, y(m))]).unwrap_or_default();

        let mut scale = format!(" {} \u{b7} {} per point", fmt_span(self.span_ms), fmt_span(width));
        if self.average > 1 {
            scale += &format!(" \u{b7} avg {}", fmt_span(width * self.average));
        }
        let block = panel("Current").title_top(Line::from(scale + " ").right_aligned().style(dim()));
        let chart = Chart::new(vec![
            Dataset::default()
                .marker(Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::new().fg(MEAN_COLOR).add_modifier(Modifier::DIM))
                .data(&mean_line),
            Dataset::default()
                .marker(Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::new().fg(TRACE_COLOR))
                .data(&plotted),
        ])
        .block(block)
        .x_axis(Axis::default().style(dim()).bounds([t0, t1]).labels(x_labels))
        .y_axis(Axis::default().style(dim()).bounds(y_bounds).labels(y_labels));
        frame.render_widget(chart, area);

        View {
            start_ms: w_start,
            end_ms: w_end,
            width_ms: width,
            mean,
            low: if low.is_finite() { low } else { 0.0 },
            peak,
        }
    }

    fn render_header(&self, frame: &mut Frame, area: Rect) {
        let bold = Style::new().add_modifier(Modifier::BOLD);
        let badge = |text: &'static str, color| Span::styled(text, bold.fg(Color::Black).bg(color));
        let state = if !self.live {
            badge(" \u{25a0} STOPPED ", Color::Gray)
        } else if self.view_end.is_some() {
            badge(" \u{25ae}\u{25ae} PAUSED ", Color::Yellow)
        } else {
            badge(" \u{25cf} LIVE ", Color::Green)
        };
        let mut header = vec![
            Span::styled(" etrace ", bold.fg(Color::Black).bg(Color::Cyan)),
            state,
            Span::styled(format!("  {}", self.source), dim()),
        ];
        let alert = bold.fg(Color::Red);
        if self.trace.overcurrent {
            header.push(Span::styled("  OVERCURRENT", alert));
        }
        if self.decoder.counters.bad_bytes > 0 {
            header.push(Span::styled(
                format!("  {} bad bytes", self.decoder.counters.bad_bytes),
                alert,
            ));
        }
        if let Some(m) = &self.message {
            header.push(Span::styled(format!("  {m}"), alert));
        }
        frame.render_widget(Paragraph::new(Line::from(header)), area);
    }

    /// One row showing everything held, with the part on screen highlighted.
    fn render_overview(&self, frame: &mut Frame, area: Rect, view: &View) {
        const BARS: [&str; 8] = [
            "\u{2581}", "\u{2582}", "\u{2583}", "\u{2584}", "\u{2585}", "\u{2586}", "\u{2587}", "\u{2588}",
        ];
        let (start, end) = (self.trace.start_ms(), self.trace.end_ms());
        let cells = (area.width as usize).saturating_sub(4);
        if end <= start || cells == 0 {
            return;
        }
        let cell_ms = (end - start) as f64 / cells as f64;
        let edge = |i: usize| start + (i as f64 * cell_ms) as u64;
        let values: Vec<Option<f64>> = (0..cells)
            .map(|i| {
                self.trace
                    .pps(edge(i), edge(i + 1).max(edge(i) + 1))
                    .map(|pps| self.cal.na(pps))
            })
            .collect();
        // Logarithmic height from 0.1 uA to at least 1 mA, so both sleep and wake-ups register.
        let top = values.iter().flatten().fold(1e6, |a: f64, b| a.max(*b)).log10();
        let bottom = LOG_FLOOR_NA.log10();
        let mut spans = vec![Span::raw("  ")];
        for (i, value) in values.iter().enumerate() {
            let on_screen = edge(i + 1) > view.start_ms && edge(i) < view.end_ms;
            let style = if on_screen { Style::new().fg(TRACE_COLOR) } else { dim() };
            let bar = match value {
                Some(na) => {
                    let level = (na.max(LOG_FLOOR_NA).log10() - bottom) / (top - bottom);
                    BARS[((level * 7.0).round() as usize).min(7)]
                }
                None => " ",
            };
            spans.push(Span::styled(bar, style));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn render_cards(&self, frame: &mut Frame, area: Rect, view: &View) {
        let newest = self.trace.end_ms();
        let now = self
            .live
            .then(|| self.trace.pps(newest.saturating_sub(1000), newest))
            .flatten()
            .map(|pps| self.cal.na(pps));
        let from = self.totals_from_ms.max(self.trace.start_ms());
        let total_s = newest.saturating_sub(from) as f64 / 1000.0;
        let total = self.trace.pps(from, newest).map(|pps| self.cal.na(pps));
        let window_s = view.end_ms.saturating_sub(view.start_ms) as f64 / 1000.0;
        let energy = |mean_na: f64, seconds: f64| fmt_energy(mean_na * seconds / 1000.0 * SUPPLY_V);

        if area.height < 5 {
            // Compact form: one line.
            let mut text = String::from(" ");
            if let Some(na) = now {
                text += &format!("now {}   ", fmt_current(na));
            }
            if let Some(mean) = view.mean {
                text += &format!(
                    "window: mean {}  min {}  max {}   ",
                    fmt_current(mean),
                    fmt_current(view.low),
                    fmt_current(view.peak)
                );
            }
            if let Some(mean) = total {
                text += &format!("total: mean {}  {}", fmt_current(mean), energy(mean, total_s));
            }
            frame.render_widget(Paragraph::new(text), area);
            return;
        }

        let value =
            |text: String, color: Color| Span::styled(text, Style::new().fg(color).add_modifier(Modifier::BOLD));
        let label = |text: &'static str| Span::styled(text, dim());
        let label_owned = |text: String| Span::styled(text, dim());
        let now_width = if self.live { 18 } else { 0 };
        let [now_area, window_area, total_area, view_area, cal_area] = Layout::horizontal([
            Constraint::Length(now_width),
            Constraint::Length(28),
            Constraint::Length(22),
            Constraint::Length(24),
            Constraint::Min(0),
        ])
        .areas(area);

        if let Some(na) = now {
            let lines = vec![
                Line::from(value(fmt_current(na), TRACE_COLOR)),
                Line::from(label("last 1 s")),
            ];
            let block = panel("Now");
            let inner = block.inner(now_area);
            frame.render_widget(Paragraph::new(lines).block(block), now_area);
            // Level meter on a logarithmic scale from 0.1 uA to 100 mA.
            let ratio = ((na.max(LOG_FLOOR_NA).log10() - 2.0) / 6.0).clamp(0.0, 1.0);
            let filled = (ratio * inner.width as f64).round() as usize;
            let meter = Line::from(vec![
                Span::styled("\u{2501}".repeat(filled), Style::new().fg(TRACE_COLOR)),
                Span::styled("\u{2500}".repeat((inner.width as usize).saturating_sub(filled)), dim()),
            ]);
            let row = Rect {
                y: inner.y + 2,
                height: inner.height.saturating_sub(2).min(1),
                ..inner
            };
            frame.render_widget(Paragraph::new(meter), row);
        } else if self.live {
            frame.render_widget(panel("Now"), now_area);
        }

        let window = match view.mean {
            Some(mean) => vec![
                Line::from(vec![label("mean "), value(fmt_current(mean), MEAN_COLOR)]),
                Line::from(vec![
                    label("min "),
                    Span::raw(fmt_current(view.low)),
                    label("  max "),
                    Span::raw(fmt_current(view.peak)),
                ]),
                Line::from(vec![
                    Span::raw(energy(mean, window_s)),
                    label(" in "),
                    Span::raw(fmt_time(window_s, 0)),
                ]),
            ],
            None => vec![Line::from(label("no data in view"))],
        };
        frame.render_widget(Paragraph::new(window).block(panel("Window")), window_area);

        let totals = match total {
            Some(mean) => vec![
                Line::from(vec![label("mean "), value(fmt_current(mean), Color::Reset)]),
                Line::from(vec![label("time "), Span::raw(fmt_time(total_s, 0))]),
                Line::from(vec![label("energy "), Span::raw(energy(mean, total_s))]),
            ],
            None => vec![Line::from(label("waiting for data"))],
        };
        let title = if self.live { "Total" } else { "Capture" };
        frame.render_widget(Paragraph::new(totals).block(panel(title)), total_area);

        let averaged_ms = view.width_ms * self.average;
        let settings = vec![
            Line::from(vec![label("span "), Span::raw(fmt_span(self.span_ms))]),
            Line::from(vec![
                label("1 pulse = "),
                Span::raw(fmt_current(self.cal.step_na(averaged_ms as f64 / 1000.0))),
            ]),
            Line::from(vec![
                label("Y "),
                Span::raw(if self.y == YScale::Auto { "auto" } else { "fixed" }),
                label(" \u{b7} "),
                Span::raw(if self.log { "log" } else { "linear" }),
            ]),
        ];
        frame.render_widget(Paragraph::new(settings).block(panel("View")), view_area);

        // "pps" = pulses per second; the help screen spells it out.
        let calibration = vec![
            Line::from(vec![
                label("scale "),
                Span::raw(format!("{:.1} nA/pps", self.cal.na_per_pps)),
            ]),
            Line::from(vec![label("offset "), Span::raw(format!("{:.1} pps", self.cal.x0_pps))]),
            Line::from(label_owned(self.cal_note.clone())),
        ];
        if cal_area.width >= 12 {
            frame.render_widget(Paragraph::new(calibration).block(panel("Calibration")), cal_area);
        }
    }

    fn render_keys(&self, frame: &mut Frame, area: Rect) {
        let mut keys: Vec<(&str, &str)> = Vec::new();
        if self.live {
            keys.push(("space", "pause"));
        }
        keys.extend([
            ("+/-", "span"),
            ("\u{2190}/\u{2192}", "pan"),
            ("\u{2191}/\u{2193}", "Y zoom"),
            ("a", "auto Y"),
            ("g", "log"),
            ("s", "smooth"),
            ("f", "fit"),
            ("?", "help"),
            ("q", "quit"),
        ]);
        let mut spans = vec![Span::raw(" ")];
        for (key, what) in keys {
            spans.push(Span::styled(
                key,
                Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(format!(" {what}   "), dim()));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    /// Run interactively until the user quits. `feed` supplies live data, if any.
    pub fn run(&mut self, feed: Option<Receiver<Feed>>) -> Result<()> {
        let mut terminal = ratatui::init();
        let result = self.event_loop(&mut terminal, feed);
        ratatui::restore();
        result
    }

    fn event_loop(&mut self, terminal: &mut DefaultTerminal, feed: Option<Receiver<Feed>>) -> Result<()> {
        loop {
            if let Some(rx) = &feed {
                self.drain(rx);
            }
            terminal.draw(|f| self.render(f))?;
            if event::poll(Duration::from_millis(40))? {
                if let TermEvent::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press && !self.key(key) {
                        return Ok(());
                    }
                }
            }
        }
    }

    /// Take everything the source has sent so far.
    pub fn drain(&mut self, rx: &Receiver<Feed>) {
        loop {
            match rx.try_recv() {
                Ok(Feed::Data(bytes)) => self.feed(&bytes),
                Ok(Feed::End(result)) => {
                    self.stopped(result.err().map(|e| e.to_string()));
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if self.live {
                        self.stopped(None);
                    }
                    break;
                }
            }
        }
    }

    fn stopped(&mut self, error: Option<String>) {
        if self.live {
            self.live = false;
            self.view_end = Some(self.end());
        }
        if error.is_some() {
            self.message = error;
        }
    }

    /// Render one frame as plain text. For tests and for looking at a trace without a terminal.
    pub fn dump(&mut self, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test backend");
        terminal.draw(|f| self.render(f)).expect("draw");
        let buffer = terminal.backend().buffer();
        buffer
            .content()
            .chunks(width as usize)
            .map(|row| {
                row.iter()
                    .map(|c| c.symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Full scale for a trace peaking at `peak` nA: the next of 1, 2, 5 x 10^k above it with some headroom.
fn full_scale_for(peak: f64) -> f64 {
    nice_step((peak * 1.1).max(MIN_FULL_SCALE_NA), 1)
}

/// A time for the X axis, with precision to suit the span being shown.
fn fmt_time(seconds: f64, span_ms: u64) -> String {
    if seconds.abs() >= 3600.0 {
        format!(
            "{}:{:02}:{:02}",
            seconds as u64 / 3600,
            seconds as u64 / 60 % 60,
            seconds as u64 % 60
        )
    } else if seconds.abs() >= 120.0 {
        format!("{}:{:04.1}", seconds as u64 / 60, seconds % 60.0)
    } else if span_ms != 0 && span_ms < 2000 {
        format!("{seconds:.2} s")
    } else {
        format!("{seconds:.1} s")
    }
}

fn fmt_span(ms: u64) -> String {
    let trim = |v: f64| format!("{v:.1}").trim_end_matches(".0").to_string();
    match ms {
        0..=999 => format!("{ms} ms"),
        1_000..=119_999 => format!("{} s", trim(ms as f64 / 1000.0)),
        _ => format!("{} min", trim(ms as f64 / 60_000.0)),
    }
}

fn fmt_energy(uj: f64) -> String {
    if uj >= 1e6 {
        format!("{:.3} J", uj / 1e6)
    } else if uj >= 1e3 {
        format!("{:.2} mJ", uj / 1e3)
    } else {
        format!("{uj:.1} \u{b5}J")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::Event;

    /// Raw stream: `ms` milliseconds at 10 kHz with one pulse every `period` ticks.
    fn stream(ms: u64, period: u64) -> Vec<u8> {
        let mut v = vec![0x34, 0x10, 0x27, 0x5c];
        for tick in 0..ms * 10 {
            if tick > 0 && tick % 256 == 0 {
                v.push(0x5c);
            }
            if tick % period == 0 {
                v.extend_from_slice(&[0x70, (tick % 256) as u8, 1, 0]);
            }
        }
        v
    }

    fn scope(ms: u64, period: u64) -> Scope {
        let cal = Calibration {
            na_per_pps: 500.0,
            x0_pps: 0.0,
        };
        Scope::file(&stream(ms, period), 10_000, cal, "test".into())
    }

    #[test]
    fn full_scale_steps() {
        assert_eq!(full_scale_for(0.0), 1_000.0);
        assert_eq!(full_scale_for(1_700.0), 2_000.0);
        assert_eq!(full_scale_for(1_900.0), 5_000.0);
        assert_eq!(full_scale_for(3_300_000.0), 5_000_000.0);
    }

    #[test]
    fn window_is_aligned_and_fits() {
        let mut s = scope(5_000, 10);
        s.span_ms = 1_000;
        let (start, width, n) = s.window(300);
        assert_eq!(width, 4);
        assert!(n <= 300 && n as u64 * width <= 1_000);
        assert_eq!(start.rem_euclid(width as i64), 0);
    }

    #[test]
    fn renders_a_steady_trace() {
        // 1 pulse per ms = 1000 pulses/s = 500 uA with this calibration.
        let mut s = scope(5_000, 10);
        let text = s.dump(100, 20);
        assert!(text.contains("STOPPED"), "{text}");
        assert!(text.contains("mean 499 µA"), "{text}"); // the first 10 records are dropped
        assert!(text.contains("span 5 s"), "{text}");
        assert!(text.contains("Capture"), "{text}");
        // The trace is drawn in braille somewhere in the chart area.
        assert!(
            text.chars()
                .any(|c| ('\u{2800}'..='\u{28ff}').contains(&c) && c != '\u{2800}'),
            "{text}"
        );
    }

    #[test]
    fn keys_change_the_view() {
        let mut s = scope(20_000, 10);
        let press = |s: &mut Scope, code| s.key(KeyEvent::new(code, KeyModifiers::NONE));
        assert_eq!(s.span_ms, 19_999);
        press(&mut s, KeyCode::Char('+'));
        assert_eq!(s.span_ms, 10_000);
        press(&mut s, KeyCode::Char('-'));
        assert_eq!(s.span_ms, 20_000);
        press(&mut s, KeyCode::Char('+'));
        press(&mut s, KeyCode::Home);
        assert_eq!(s.end(), 10_000);
        press(&mut s, KeyCode::Right);
        assert_eq!(s.end(), 11_000);
        press(&mut s, KeyCode::Char('g'));
        assert!(s.log);
        press(&mut s, KeyCode::Down);
        assert!(matches!(s.y, YScale::Fixed(_)));
        assert!(!press(&mut s, KeyCode::Char('q')));
    }

    #[test]
    fn averaging_smooths_sparse_pulses() {
        // One pulse every 50 ms: without averaging most points read zero and a few read high.
        let mut s = scope(10_000, 500);
        s.set_span(2.0);
        s.set_end(6.0);
        let plain = s.dump(100, 20);
        assert!(plain.contains("min 0.00 µA"), "{plain}");
        for _ in 0..3 {
            s.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        }
        let smooth = s.dump(100, 20);
        // 20 pulses/s * 500 nA = 10 uA, give or take one pulse in the averaging window.
        assert!(!smooth.contains("min 0.00 µA"), "{smooth}");
        assert!(smooth.contains("avg 768 ms"), "{smooth}");
    }

    #[test]
    fn help_opens_and_any_key_closes_it() {
        let mut s = scope(5_000, 10);
        s.key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
        assert!(s.dump(100, 30).contains("any key closes"));
        // The key that closes the help does nothing else.
        assert!(s.key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)));
        assert!(!s.dump(100, 30).contains("any key closes"));
    }

    #[test]
    fn short_terminal_gets_a_compact_readout() {
        let text = scope(5_000, 10).dump(100, 12);
        assert!(text.contains("window: mean 499 µA"), "{text}");
        assert!(!text.contains("Capture"), "{text}");
    }

    #[test]
    fn live_scope_follows_then_pauses() {
        let cal = Calibration {
            na_per_pps: 500.0,
            x0_pps: 0.0,
        };
        let mut s = Scope::live(10_000, cal, "test".into());
        s.feed(&stream(3_000, 10));
        assert_eq!(s.end(), s.trace.end_ms());
        s.key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        let frozen = s.end();
        s.trace.push(Event::Tick { tick: 60_000 });
        assert_eq!(s.end(), frozen);
        s.key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert_eq!(s.end(), 6_000);
        assert!(s.dump(100, 20).contains("LIVE"));
    }
}
