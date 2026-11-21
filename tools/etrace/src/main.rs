use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::{Args, Parser, Subcommand};

use energytrace::cal::{Calibration, Meta};
use energytrace::decode::Decoder;
use energytrace::session::{Log, Logger, Session};
use energytrace::trace::Trace;
use etrace::board::{self, Probe};
use etrace::flash::{flash, Step};
use etrace::scope::{Feed, Scope};
use etrace::{plot, Error, Result};
use xds110_sleep::{self as sleep, Dap};

/// EnergyTrace current measurement with a TI XDS110 probe, and debug access
/// to MSPM0 chips that are asleep.
///
/// The probe on a LaunchPad powers the target and counts the charge it
/// delivers. This tool reads that count: live as a terminal oscilloscope, or
/// headless into a capture file with statistics.
///
/// A sleeping MSPM0 does not answer a debugger, and a chip that a debugger
/// has touched does not sleep. `wake`, `release` and `flash` deal with both;
/// `measure` goes from an image to a current measurement in one step.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(flatten)]
    select: SelectArgs,
    #[command(subcommand)]
    command: Command,
}

#[derive(Args)]
struct SelectArgs {
    /// LaunchPad to use when several are plugged in: l1306 or c1104
    #[arg(long, global = true, env = "BOARD", value_name = "NAME")]
    board: Option<String>,
    /// Probe to use, by serial number or its first characters (wins over --board)
    #[arg(long, global = true, env = "ETRACE_SERIAL", value_name = "SERIAL")]
    serial: Option<String>,
}

impl SelectArgs {
    fn probe(&self) -> Result<Probe> {
        board::select(self.board.as_deref(), self.serial.as_deref())
    }
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
    /// Flash an image if given, let the chip go, and record its current
    Measure(MeasureArgs),
    /// Write an image to flash, also when the chip is asleep, then let it run
    Flash(FlashArgs),
    /// Show the debug port and whether the chip is asleep (changes nothing)
    Status(DapArgs),
    /// Force a sleeping chip awake so that a debugger can attach
    Wake(DapArgs),
    /// Undo `wake` and any debugger hold, then reset: do this before measuring
    Release(DapArgs),
    /// Pulse NRST
    Reset(DapArgs),
    /// Wake and halt the chip; print where it is and its sleep configuration
    Dump(DapArgs),
    /// Reset, then report how long the chip stays reachable
    Watch(WatchArgs),
    /// Halt a chip that crashes at boot, right after reset
    Catch(CatchArgs),
    /// List the XDS110 probes that are plugged in
    Probes,
}

#[derive(Args)]
struct DapArgs {
    /// Print every SWD transfer
    #[arg(long)]
    verbose: bool,
}

#[derive(Args)]
struct WatchArgs {
    /// How long to watch, in seconds
    #[arg(default_value_t = 20.0)]
    secs: f64,
    #[command(flatten)]
    dap: DapArgs,
}

#[derive(Args)]
struct CatchArgs {
    /// Then run on for up to 6 s and print the registers at a HardFault
    #[arg(long)]
    trace: bool,
    #[command(flatten)]
    dap: DapArgs,
}

#[derive(Args)]
struct FlashArgs {
    /// ELF file, or a raw binary for address 0 if the name ends in .bin
    image: PathBuf,
    /// Chip name for probe-rs, if the board is not one this tool knows
    #[arg(long)]
    chip: Option<String>,
    #[command(flatten)]
    dap: DapArgs,
}

#[derive(Args)]
struct MeasureArgs {
    /// Raw capture to write; calibration goes next to it as <FILE>.json
    file: PathBuf,
    /// Image to flash first (default: measure what is on the chip)
    #[arg(long, value_name = "IMAGE")]
    flash: Option<PathBuf>,
    /// Chip name for probe-rs, if the board is not one this tool knows
    #[arg(long)]
    chip: Option<String>,
    /// Length of the capture, in seconds
    #[arg(long, default_value_t = 10.0)]
    secs: f64,
    /// Wait this long after the reset before measuring, in seconds
    #[arg(long, default_value_t = 5.0)]
    settle: f64,
    /// Do not touch SWD or NRST (for a board with its debug jumpers pulled)
    #[arg(long, conflicts_with = "flash")]
    no_swd: bool,
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

/// `--verbose` for the EnergyTrace commands: every command and stream packet, on stderr.
fn session_logger(verbose: bool) -> Option<Logger> {
    verbose.then(|| -> Logger {
        Box::new(|entry| match entry {
            Log::Drained { endpoint, bytes } => {
                eprintln!("drained {bytes} stale bytes from endpoint {endpoint:#04x}")
            }
            Log::Command { name, tx, rx, elapsed } => eprintln!(
                "{name}: tx {tx:02x?}  rx {:02x?}  ({} ms)",
                &rx[..rx.len().min(24)],
                elapsed.as_millis()
            ),
            Log::Stream { bytes, at } => eprintln!("stream: {bytes} bytes at {:.3} s", at.as_secs_f64()),
        })
    })
}

fn open_session(probe: &Probe, args: &ProbeArgs, default_rate: u32) -> Result<Session> {
    eprintln!("calibrating...");
    let session = Session::open_logged(
        &probe.serial,
        args.rate.unwrap_or(default_rate),
        session_logger(args.verbose),
    )?;
    eprintln!(
        "XDS110 hardware {:#04x}, firmware {}; {}",
        session.info.hardware,
        session.info.firmware_string(),
        describe_cal(&session.cal)
    );
    Ok(session)
}

fn capture(probe: &Probe, args: CaptureArgs) -> Result<()> {
    let stop = interrupt_flag();
    let mut session = open_session(probe, &args.probe, CAPTURE_RATE)?;
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
    session.pump::<Error>(&stop, |bytes| {
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
            if let energytrace::Event::Config { rate: r } = e {
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

fn live(select: &SelectArgs, args: LiveArgs) -> Result<()> {
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
        let mut session = open_session(&select.probe()?, &args.probe, LIVE_RATE)?;
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
            let result = session.pump::<Error>(&flag, |bytes| {
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

fn cal(probe: &Probe, args: ProbeArgs) -> Result<()> {
    let session = Session::open_logged(
        &probe.serial,
        args.rate.unwrap_or(CAPTURE_RATE),
        session_logger(args.verbose),
    )?;
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

/// Chip name for probe-rs: the one given, or the one on the board the probe belongs to.
fn chip_name(probe: &Probe, given: Option<String>) -> Result<String> {
    given.or(probe.board.map(|b| b.chip.to_string())).ok_or_else(|| {
        Error(format!(
            "the probe's serial number ({}) does not name a board this tool knows: give --chip or --board",
            probe.serial
        ))
    })
}

/// Open the probe's debug port; `--verbose` prints every SWD transfer on stderr.
fn open_dap(probe: &Probe, verbose: bool) -> Result<Dap> {
    let mut dap = Dap::open(probe)?;
    if verbose {
        dap.set_trace(Some(Box::new(|t| {
            eprintln!(
                "   transfer req={:#04x} data={:x?} -> count={} ack={} {:02x?}",
                t.request, t.data, t.count, t.ack, t.response
            )
        })));
    }
    Ok(dap)
}

fn print_woken(w: &sleep::Woken) {
    println!(
        "DPREC0 {:#010x} -> {:#010x}, AHB-AP IDR {:#010x}: awake",
        w.dprec0_before, w.dprec0_after, w.ahb_ap_idr
    );
}

fn print_released(r: &sleep::Released) {
    if !r.halt_withdrawn {
        println!("warning: could not reach DHCSR; relying on the reset to clear debug state");
    }
    println!("released and reset");
}

fn flash_image(probe: &Probe, chip: &str, image: &Path, verbose: bool) -> Result<()> {
    flash(
        probe,
        chip,
        image,
        || open_dap(probe, verbose),
        |step| match step {
            Step::Woken(w) => print_woken(&w),
            Step::ProbeRs(text) if verbose => eprint!("{text}"),
            Step::ProbeRs(_) => {}
            Step::Released(r) => print_released(&r),
        },
    )
}

fn measure(probe: &Probe, args: MeasureArgs) -> Result<()> {
    if let Some(image) = &args.flash {
        let chip = chip_name(probe, args.chip)?;
        eprintln!("flashing {}...", image.display());
        flash_image(probe, &chip, image, args.probe.verbose)?;
    } else if !args.no_swd {
        print_released(&sleep::release(&mut open_dap(probe, args.probe.verbose)?)?);
    }
    // Every firmware here stays awake for a few seconds after reset so that a debugger can attach.
    std::thread::sleep(Duration::from_secs_f64(args.settle));
    capture(
        probe,
        CaptureArgs {
            file: args.file,
            secs: Some(args.secs),
            probe: args.probe,
            out: args.out,
            png: args.png,
            svg: args.svg,
        },
    )
}

fn status(d: &mut Dap) -> Result<()> {
    let s = sleep::status(d)?;
    println!("DPIDR            {:#010x}", s.dpidr);
    println!("CTRL/STAT        {:#010x}", s.ctrl_stat);
    for (ap, idr) in s.ap_idr.iter().enumerate() {
        match idr {
            Some(idr) => println!("AP{ap} IDR          {idr:#010x}"),
            None => println!("AP{ap} IDR          FAULT"),
        }
    }
    println!(
        "PWR-AP DPREC0    {:#010x}  (FORCEACTIVE {})",
        s.dprec0,
        if s.force_active() { "set" } else { "clear" }
    );
    println!(
        "=> AHB-AP {}",
        if s.asleep {
            "power-gated: chip is in STOP/STANDBY; use `wake`"
        } else {
            "reachable"
        }
    );
    Ok(())
}

fn dump(d: &mut Dap) -> Result<()> {
    let dump = sleep::dump(d).map_err(|e| match e {
        sleep::Error::NotResponding { .. } => "the AHB-AP does not answer, even right after reset".into(),
        e => Error::from(e),
    })?;
    print_woken(&dump.woken);
    let fixed = [("DHCSR", dump.dhcsr), ("PC", dump.pc), ("LR", dump.lr), ("SP", dump.sp)];
    for (name, value) in fixed.iter().chain(&dump.registers) {
        println!("{name:<16} {value:#010x}");
    }
    println!(
        "{:<16} {:#010x} {:#010x}{}",
        "flash[0],[4]",
        dump.vectors[0],
        dump.vectors[1],
        if dump.flash_blank() {
            "  <- BLANK: ROM BSL will run"
        } else {
            ""
        }
    );
    let [a, b, c] = dump.nonmain;
    println!("{:<16} {a:08x} {b:08x} {c:08x}", "NONMAIN[0..3]");
    println!("(core left halted; run `release` when done)");
    Ok(())
}

fn watch(d: &mut Dap, secs: f64) -> Result<()> {
    sleep::watch(d, Duration::from_secs_f64(secs), |e| {
        println!(
            "t={:5.1}s  AHB-AP {:<9} DPREC0={:#010x}",
            e.at.as_secs_f64(),
            if e.reachable { "reachable" } else { "FAULT" },
            e.dprec0
        )
    })?;
    Ok(())
}

fn catch(d: &mut Dap, trace: bool) -> Result<()> {
    sleep::catch(d)?;
    // Only these two: at the reset vector the stack pointer is at the end of RAM.
    println!(
        "halted: PC {:#x} SP {:#x}",
        sleep::core_register(d, 15)?,
        sleep::core_register(d, 13)?
    );
    if !trace {
        return Ok(());
    }
    match sleep::run_to_hardfault(d, Duration::from_secs(6))? {
        sleep::RunOutcome::Halted { after, registers: r } => {
            println!(
                "t={:.1} halted: PC {:#x} LR {:#x} SP {:#x} xPSR {:#x}",
                after.as_secs_f64(),
                r.pc,
                r.lr,
                r.sp,
                r.xpsr
            );
            let stack: Vec<String> = r.stack.iter().map(|w| format!("{w:08x}")).collect();
            println!("stack: {}", stack.join(" "));
        }
        sleep::RunOutcome::LostAccess { after } => println!("t={:.1} lost access", after.as_secs_f64()),
        sleep::RunOutcome::StillRunning { registers: r } => {
            println!("still running after 6 s: PC {:#x} SP {:#x}", r.pc, r.sp)
        }
    }
    Ok(())
}

fn probes() -> Result<()> {
    let all = sleep::probe::list();
    if all.is_empty() {
        return Err("no XDS110 probe found on USB".into());
    }
    for p in all {
        match board::board_of(&p.serial) {
            Some(b) => println!("{}  {}  ({})", p.serial, b.name, b.chip),
            None => println!("{}  unknown board", p.serial),
        }
    }
    Ok(())
}

fn main() {
    let Cli { select, command } = Cli::parse();
    let dap = |args: &DapArgs| open_dap(&select.probe()?, args.verbose);
    let result = match command {
        Command::Live(a) => live(&select, a),
        Command::View(a) => view(a),
        Command::Capture(a) => select.probe().and_then(|p| capture(&p, a)),
        Command::Stats(a) => stats(a),
        Command::Export(a) => export(a),
        Command::Cal(a) => select.probe().and_then(|p| cal(&p, a)),
        Command::Measure(a) => select.probe().and_then(|p| measure(&p, a)),
        Command::Flash(a) => select.probe().and_then(|p| {
            let chip = chip_name(&p, a.chip)?;
            flash_image(&p, &chip, &a.image, a.dap.verbose)
        }),
        Command::Status(a) => dap(&a).and_then(|mut d| status(&mut d)),
        Command::Wake(a) => dap(&a).and_then(|mut d| {
            print_woken(&etrace::wake(&mut d)?);
            Ok(())
        }),
        Command::Release(a) => dap(&a).and_then(|mut d| {
            print_released(&sleep::release(&mut d)?);
            Ok(())
        }),
        Command::Reset(a) => dap(&a).and_then(|mut d| {
            sleep::reset(&mut d)?;
            println!("NRST pulsed");
            Ok(())
        }),
        Command::Dump(a) => dap(&a).and_then(|mut d| dump(&mut d)),
        Command::Watch(a) => dap(&a.dap).and_then(|mut d| watch(&mut d, a.secs)),
        Command::Catch(a) => dap(&a.dap).and_then(|mut d| catch(&mut d, a.trace)),
        Command::Probes => probes(),
    };
    if let Err(e) = result {
        eprintln!("etrace: {e}");
        std::process::exit(1);
    }
}
