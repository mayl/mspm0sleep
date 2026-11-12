use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::{Args, Parser, Subcommand};

use etrace::cal::{Calibration, Meta};
use etrace::decode::Decoder;
use etrace::probe::Session;
use etrace::scope::{Feed, Scope};
use etrace::trace::Trace;
use etrace::{plot, Error, Result};

/// EnergyTrace current measurement with a TI XDS110 probe.
///
/// The probe on a LaunchPad powers the target and counts the charge it
/// delivers. This tool reads that count: live as a terminal oscilloscope, or
/// headless into a capture file with statistics.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Watch the current live in a terminal oscilloscope
    Live(LiveArgs),
    /// Open a saved capture in the oscilloscope
    View(ViewArgs),
    /// Record a capture without a display and print its statistics
    Capture(CaptureArgs),
    /// Print statistics for saved captures
    Stats(StatsArgs),
    /// Write a saved capture as CSV, SVG or PNG
    Export(ExportArgs),
    /// Run the probe's calibration and print the result
    Cal(ProbeArgs),
}

#[derive(Args)]
struct ProbeArgs {
    /// Probe time resolution in ticks per second [default: 10000; 50000 for `live`]
    #[arg(long)]
    rate: Option<u32>,
    /// Print every command sent to the probe
    #[arg(long)]
    verbose: bool,
}

#[derive(Args)]
struct CalArgs {
    /// Calibration to use instead of the capture's own: nA per pulse/s, then the open-load pulses/s
    #[arg(long, value_name = "NA_PER_PPS,X0_PPS", value_parser = parse_cal)]
    cal: Option<Calibration>,
}

#[derive(Args)]
struct DisplayArgs {
    /// Time span to show, in seconds
    #[arg(long, value_name = "SECONDS")]
    span: Option<f64>,
    /// Start with a logarithmic current axis
    #[arg(long)]
    log: bool,
    /// Print one frame of this size as text instead of opening the display, e.g. 120x30
    #[arg(long, value_name = "WxH", value_parser = parse_size)]
    dump: Option<(u16, u16)>,
}

impl DisplayArgs {
    fn apply(&self, scope: &mut Scope) {
        if let Some(span) = self.span {
            scope.set_span(span);
        }
        scope.set_log(self.log);
    }
}

#[derive(Args)]
struct LiveArgs {
    #[command(flatten)]
    probe: ProbeArgs,
    /// Also record the raw stream to this file
    #[arg(long, value_name = "FILE")]
    save: Option<PathBuf>,
    /// Play back a saved capture in real time instead of using the probe
    #[arg(long, value_name = "FILE")]
    replay: Option<PathBuf>,
    /// Playback speed for --replay
    #[arg(long, default_value_t = 1.0)]
    speed: f64,
    #[command(flatten)]
    cal: CalArgs,
    #[command(flatten)]
    display: DisplayArgs,
    /// With --dump: collect data for this many seconds first
    #[arg(long, default_value_t = 3.0)]
    secs: f64,
}

#[derive(Args)]
struct ViewArgs {
    file: PathBuf,
    #[command(flatten)]
    cal: CalArgs,
    #[command(flatten)]
    display: DisplayArgs,
    /// Put the right edge of the view at this time, in seconds from the start
    #[arg(long, value_name = "SECONDS")]
    at: Option<f64>,
}

#[derive(Args)]
struct CaptureArgs {
    /// Raw capture to write; calibration goes next to it as <FILE>.json
    file: PathBuf,
    /// Stop after this many seconds (default: until Ctrl-C)
    #[arg(long)]
    secs: Option<f64>,
    #[command(flatten)]
    probe: ProbeArgs,
    #[command(flatten)]
    out: OutputArgs,
    /// Also write a PNG next to the capture
    #[arg(long)]
    png: bool,
    /// Also write an SVG next to the capture
    #[arg(long)]
    svg: bool,
}

#[derive(Args)]
struct OutputArgs {
    /// Window length for the percentile statistics, in milliseconds
    #[arg(long, default_value_t = 100)]
    bin_ms: u64,
    /// Print the statistics as JSON
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct StatsArgs {
    #[arg(required = true)]
    files: Vec<PathBuf>,
    #[command(flatten)]
    cal: CalArgs,
    #[command(flatten)]
    out: OutputArgs,
}

#[derive(Args)]
struct ExportArgs {
    file: PathBuf,
    #[arg(long, value_name = "FILE")]
    csv: Option<PathBuf>,
    #[arg(long, value_name = "FILE")]
    svg: Option<PathBuf>,
    #[arg(long, value_name = "FILE")]
    png: Option<PathBuf>,
    /// Time resolution of the output, in milliseconds
    #[arg(long, default_value_t = 10)]
    bin_ms: u64,
    #[command(flatten)]
    cal: CalArgs,
}

fn parse_cal(s: &str) -> std::result::Result<Calibration, String> {
    let (k, x0) = s.split_once(',').ok_or("expected NA_PER_PPS,X0_PPS, e.g. 493.5,16.2")?;
    Ok(Calibration {
        na_per_pps: k.trim().parse().map_err(|_| format!("not a number: {k}"))?,
        x0_pps: x0.trim().parse().map_err(|_| format!("not a number: {x0}"))?,
    })
}

fn parse_size(s: &str) -> std::result::Result<(u16, u16), String> {
    let (w, h) = s.split_once('x').ok_or("expected WIDTHxHEIGHT, e.g. 120x30")?;
    Ok((
        w.parse().map_err(|_| "bad width")?,
        h.parse().map_err(|_| "bad height")?,
    ))
}

/// A decoded capture file and the calibration to read it with.
struct Capture {
    raw: Vec<u8>,
    trace: Trace,
    decoder: Decoder,
    cal: Calibration,
    /// Where the calibration came from, for the user.
    cal_source: &'static str,
}

fn load(path: &Path, cal: &CalArgs) -> Result<Capture> {
    let raw = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let meta = Meta::load(path);
    let (cal, cal_source) = match (cal.cal, &meta) {
        (Some(c), _) => (c, "from --cal"),
        (None, Some(m)) => (m.calibration, "saved with the capture"),
        (None, None) => (Calibration::NOMINAL, "nominal (none saved)"),
    };
    let (trace, decoder) = Trace::from_raw(&raw, meta.map_or(10_000, |m| m.sample_rate));
    Ok(Capture {
        raw,
        trace,
        decoder,
        cal,
        cal_source,
    })
}

fn describe_cal(cal: &Calibration) -> String {
    format!(
        "{:.1} nA per pulse/s, offset {:.1} pulses/s",
        cal.na_per_pps, cal.x0_pps
    )
}

fn print_stats(name: &str, trace: &Trace, decoder: &Decoder, cal: &Calibration, out: &OutputArgs) {
    let summary = trace.summary(cal, out.bin_ms);
    if out.json {
        let mut v = serde_json::to_value(summary).expect("summary serializes");
        v["file"] = name.into();
        v["calibration"] = serde_json::to_value(cal).expect("calibration serializes");
        v["bad_bytes"] = decoder.counters.bad_bytes.into();
        v["overcurrent"] = trace.overcurrent.into();
        println!("{v}");
    } else {
        println!("{name}: {summary}");
        if decoder.counters.bad_bytes > 0 {
            println!("{name}: {} bytes of bad data in the stream", decoder.counters.bad_bytes);
        }
        if trace.overcurrent {
            println!("{name}: the probe reported overcurrent");
        }
    }
}

/// Stop flag set by Ctrl-C.
fn interrupt_flag() -> Arc<AtomicBool> {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let _ = ctrlc::set_handler(move || flag.store(true, Ordering::SeqCst));
    stop
}

/// Tick rate for headless captures.
const CAPTURE_RATE: u32 = 10_000;
/// Tick rate for the live display. The probe sends a packet only when it has about 188 bytes, and
/// while the target sleeps most of those are time-keeping bytes, one per 256 ticks. A faster tick
/// fills packets sooner: the longest wait for new data drops from about 1.5 s to about 0.7 s.
const LIVE_RATE: u32 = 50_000;

fn open_session(args: &ProbeArgs, default_rate: u32) -> Result<Session> {
    eprintln!("calibrating...");
    let session = Session::open(args.rate.unwrap_or(default_rate), args.verbose)?;
    eprintln!(
        "XDS110 hardware {:#04x}, firmware {}; {}",
        session.info.hardware,
        session.info.firmware_string(),
        describe_cal(&session.cal)
    );
    Ok(session)
}

/// Read the probe until `stop` is set, passing each chunk to `sink`. Stops the probe before returning.
fn pump(session: &mut Session, stop: &AtomicBool, mut sink: impl FnMut(&[u8]) -> Result<()>) -> Result<()> {
    let mut buf = vec![0u8; 64 * 1024];
    session.start()?;
    let run: Result<()> = (|| {
        while !stop.load(Ordering::SeqCst) {
            let n = session.read(&mut buf, Duration::from_millis(200))?;
            if n > 0 {
                sink(&buf[..n])?;
            }
        }
        Ok(())
    })();
    let tail = session.stop();
    run?;
    sink(&tail?)
}

fn capture(args: CaptureArgs) -> Result<()> {
    let stop = interrupt_flag();
    let mut session = open_session(&args.probe, CAPTURE_RATE)?;
    let mut file = File::create(&args.file).map_err(|e| format!("{}: {e}", args.file.display()))?;
    session.meta().save(&args.file)?;

    let mut trace = Trace::new(session.rate);
    let mut decoder = Decoder::new();
    let deadline = args.secs.map(|s| (s * session.rate as f64) as u64);
    match args.secs {
        Some(s) => eprintln!("capturing {s} s..."),
        None => eprintln!("capturing until Ctrl-C..."),
    }
    let flag = stop.clone();
    let mut done = false;
    pump(&mut session, &stop, |bytes| {
        // Once the requested length is reached, what is still in flight is dropped, so the capture
        // ends with the packet that crossed it. That packet can hold over a second of data when the
        // target is asleep, so a timed capture may run that much long; its true length is reported.
        if done {
            return Ok(());
        }
        file.write_all(bytes)?;
        decoder.feed(bytes, |e| trace.push(e));
        // The capture length is measured on the probe's clock, not the host's.
        if deadline.is_some_and(|d| decoder.now() >= d) {
            done = true;
            flag.store(true, Ordering::SeqCst);
        }
        Ok(())
    })?;
    let cal = session.cal;
    drop(session);

    let name = args.file.display().to_string();
    print_stats(&name, &trace, &decoder, &cal, &args.out);
    if args.png {
        plot::write_png(&args.file.with_extension("png"), &trace, &cal, 10)?;
    }
    if args.svg {
        plot::write_svg(&args.file.with_extension("svg"), &trace, &cal, 10, &name)?;
    }
    Ok(())
}

/// Feed a saved capture to the scope at the speed it was recorded.
fn replay(raw: Vec<u8>, speed: f64, tx: Sender<Feed>, stop: Arc<AtomicBool>) {
    let mut pace = Decoder::new();
    let mut rate = 10_000.0;
    let started = Instant::now();
    for chunk in raw.chunks(64) {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        pace.feed(chunk, |e| {
            if let etrace::decode::Event::Config { rate: r } = e {
                rate = r.max(1) as f64;
            }
        });
        let due = Duration::from_secs_f64(pace.now() as f64 / rate / speed.max(0.01));
        if let Some(wait) = due.checked_sub(started.elapsed()) {
            std::thread::sleep(wait);
        }
        if tx.send(Feed::Data(chunk.to_vec())).is_err() {
            return;
        }
    }
    let _ = tx.send(Feed::End(Ok(())));
}

fn live(args: LiveArgs) -> Result<()> {
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();

    let (mut scope, worker) = if let Some(path) = &args.replay {
        let capture = load(path, &args.cal)?;
        let mut scope = Scope::live(
            capture.trace.rate(),
            capture.cal,
            format!("replay of {}", path.display()),
        );
        scope.set_cal_note(capture.cal_source);
        let (speed, flag) = (args.speed, stop.clone());
        (scope, std::thread::spawn(move || replay(capture.raw, speed, tx, flag)))
    } else {
        let mut session = open_session(&args.probe, LIVE_RATE)?;
        let mut file = match &args.save {
            Some(path) => {
                session.meta().save(path)?;
                Some(File::create(path).map_err(|e| format!("{}: {e}", path.display()))?)
            }
            None => None,
        };
        let cal = args.cal.cal.unwrap_or(session.cal);
        let mut source = match session.rate % 1000 {
            0 => format!("XDS110 \u{b7} {} kHz", session.rate / 1000),
            _ => format!("XDS110 \u{b7} {} Hz", session.rate),
        };
        if let Some(path) = &args.save {
            source += &format!(" \u{b7} recording to {}", path.display());
        }
        let mut scope = Scope::live(session.rate, cal, source);
        scope.set_cal_note(if args.cal.cal.is_some() {
            "from --cal"
        } else {
            "measured at start"
        });
        let flag = stop.clone();
        let worker = std::thread::spawn(move || {
            let result = pump(&mut session, &flag, |bytes| {
                if let Some(f) = &mut file {
                    f.write_all(bytes)?;
                }
                tx.send(Feed::Data(bytes.to_vec()))
                    .map_err(|_| Error::from("display closed"))
            });
            drop(session);
            let _ = tx.send(Feed::End(result));
        });
        (scope, worker)
    };

    args.display.apply(&mut scope);
    let result = match args.display.dump {
        Some((w, h)) => {
            std::thread::sleep(Duration::from_secs_f64(args.secs));
            scope.drain(&rx);
            println!("{}", scope.dump(w, h));
            Ok(())
        }
        None => scope.run(Some(rx)),
    };
    stop.store(true, Ordering::SeqCst);
    let _ = worker.join();
    result
}

fn view(args: ViewArgs) -> Result<()> {
    let capture = load(&args.file, &args.cal)?;
    let rate = capture.trace.rate();
    let mut scope = Scope::file(&capture.raw, rate, capture.cal, args.file.display().to_string());
    scope.set_cal_note(capture.cal_source);
    args.display.apply(&mut scope);
    if let Some(at) = args.at {
        scope.set_end(at);
    }
    match args.display.dump {
        Some((w, h)) => {
            println!("{}", scope.dump(w, h));
            Ok(())
        }
        None => scope.run(None),
    }
}

fn stats(args: StatsArgs) -> Result<()> {
    for path in &args.files {
        let c = load(path, &args.cal)?;
        if !args.out.json && c.cal_source.starts_with("nominal") {
            eprintln!("{}: using {} ({})", path.display(), describe_cal(&c.cal), c.cal_source);
        }
        print_stats(&path.display().to_string(), &c.trace, &c.decoder, &c.cal, &args.out);
    }
    Ok(())
}

fn export(args: ExportArgs) -> Result<()> {
    if args.csv.is_none() && args.svg.is_none() && args.png.is_none() {
        return Err("nothing to write: give --csv, --svg or --png".into());
    }
    let c = load(&args.file, &args.cal)?;
    if let Some(path) = &args.csv {
        plot::write_csv(path, &c.trace, &c.cal, args.bin_ms)?;
    }
    if let Some(path) = &args.svg {
        plot::write_svg(path, &c.trace, &c.cal, args.bin_ms, &args.file.display().to_string())?;
    }
    if let Some(path) = &args.png {
        plot::write_png(path, &c.trace, &c.cal, args.bin_ms)?;
    }
    Ok(())
}

fn cal(args: ProbeArgs) -> Result<()> {
    let session = Session::open(args.rate.unwrap_or(CAPTURE_RATE), args.verbose)?;
    println!("probe hardware   {:#06x}", session.info.hardware);
    println!("probe firmware   {}", session.info.firmware_string());
    println!(
        "reference load   {} pulses in {} ticks",
        session.cal_loaded.0, session.cal_loaded.1
    );
    println!(
        "no load          {} pulses in {} ticks",
        session.cal_open.0, session.cal_open.1
    );
    println!("scale            {:.2} nA per pulse/s", session.cal.na_per_pps);
    println!(
        "offset           {:.2} pulses/s ({:.2} uA)",
        session.cal.x0_pps,
        session.cal.x0_pps * session.cal.na_per_pps / 1000.0
    );
    Ok(())
}

fn main() {
    let result = match Cli::parse().command {
        Command::Live(a) => live(a),
        Command::View(a) => view(a),
        Command::Capture(a) => capture(a),
        Command::Stats(a) => stats(a),
        Command::Export(a) => export(a),
        Command::Cal(a) => cal(a),
    };
    if let Err(e) = result {
        eprintln!("etrace: {e}");
        std::process::exit(1);
    }
}
