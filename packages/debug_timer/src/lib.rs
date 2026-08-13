#[cfg(feature = "enable")]
mod real_debug_timer {
    use std::fmt::Write as _;
    use std::io::{stdout, Write};
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};

    const SECOND: Duration = Duration::from_secs(1);
    const MILLISECOND: Duration = Duration::from_millis(1);
    const MICROSECOND: Duration = Duration::from_micros(1);

    /// Where a timing line goes, decided once from the environment.
    ///
    /// Off by default, because the feature that compiles this in travels with
    /// the inspector and the inspector now ships: a released binary must not
    /// pay a `format!` and a locked write on every frame for a line nobody is
    /// reading, and must not spray it over the application's own stdout.
    ///
    /// `BLITZ_PHASE_TIMES=1` turns it on, `BLITZ_PHASE_TIMES_FILE=<path>`
    /// appends there instead of stdout. Same shape as `BLITZ_FRAME_STATS` and
    /// `BLITZ_FRAME_STATS_FILE` in blitz-shell.
    enum Sink {
        Off,
        Stdout,
        #[cfg(not(target_arch = "wasm32"))]
        File(Mutex<std::fs::File>),
    }

    fn sink() -> &'static Sink {
        static SINK: OnceLock<Sink> = OnceLock::new();
        SINK.get_or_init(|| {
            #[cfg(not(target_arch = "wasm32"))]
            {
                if let Some(path) = std::env::var_os("BLITZ_PHASE_TIMES_FILE") {
                    return match std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&path)
                    {
                        Ok(file) => Sink::File(Mutex::new(file)),
                        // Falling back to stdout rather than to silence: the
                        // variable is an explicit request for the lines, and a
                        // bad path should not read as "the timings are gone".
                        Err(err) => {
                            eprintln!("[blitz] BLITZ_PHASE_TIMES_FILE {path:?}: {err}");
                            Sink::Stdout
                        }
                    };
                }
                if std::env::var_os("BLITZ_PHASE_TIMES").is_some() {
                    return Sink::Stdout;
                }
                Sink::Off
            }
            #[cfg(target_arch = "wasm32")]
            Sink::Stdout
        })
    }

    /// Whether anything will be written. Callers check this *before* building
    /// their message: the string that describes a resolve costs more to format
    /// than the timer costs to keep.
    pub fn is_logging() -> bool {
        !matches!(sink(), Sink::Off)
    }

    /// Write one already-formatted line to whatever sink is configured, so a
    /// caller with something extra to say lands in the same file as the timings
    /// rather than on the application's stdout.
    pub fn write_line(line: &str) {
        match sink() {
            Sink::Off => {}
            Sink::Stdout => {
                let mut out = stdout().lock();
                let _ = out.write_all(line.as_bytes());
            }
            #[cfg(not(target_arch = "wasm32"))]
            Sink::File(file) => {
                if let Ok(mut file) = file.lock() {
                    let _ = file.write_all(line.as_bytes());
                }
            }
        }
    }

    pub struct DebugTimer {
        recorded_times: Vec<(&'static str, Instant)>,
    }

    fn value_and_units(duration: Duration) -> (f32, &'static str) {
        if duration < MICROSECOND {
            (duration.subsec_nanos() as f32, "ns")
        } else if duration < MILLISECOND {
            (duration.subsec_nanos() as f32 / 1000.0, "us")
        } else if duration < SECOND {
            (duration.subsec_micros() as f32 / 1000.0, "ms")
        } else {
            (duration.as_millis() as f32 / 1000.0, "s")
        }
    }

    impl DebugTimer {
        pub fn init() -> Self {
            Self::init_if(true)
        }

        pub fn init_if(enabled: bool) -> Self {
            Self {
                recorded_times: if enabled {
                    vec![("start", Instant::now())]
                } else {
                    Vec::new()
                },
            }
        }

        pub fn record_time(&mut self, message: &'static str) {
            if !self.recorded_times.is_empty() {
                self.recorded_times.push((message, Instant::now()));
            }
        }

        pub fn is_logging(&self) -> bool {
            is_logging()
        }

        pub fn print_times(&self, message: &str) {
            if self.recorded_times.is_empty() || !is_logging() {
                return;
            }

            let now = Instant::now();
            let (overall_val, overall_unit) = value_and_units(now - self.recorded_times[0].1);

            // Built whole and written once. A line assembled with a dozen
            // `write!`s into a held lock is a dozen chances to interleave with
            // whatever else the process is printing, and the frame pays for
            // every one of them.
            let mut line = String::with_capacity(128);
            if overall_val < 10.0 {
                let _ = write!(line, "{message}{overall_val:.1}{overall_unit} (");
            } else {
                let _ = write!(line, "{message}{overall_val:.0}{overall_unit} (");
            }

            for (idx, times) in self.recorded_times.windows(2).enumerate() {
                let last = times[0];
                let current = times[1];

                if idx != 0 {
                    let _ = write!(line, ", ");
                }

                let duration = current.1.duration_since(last.1);

                let (val, unit) = value_and_units(duration);
                if val < 10.0 {
                    let _ = write!(line, "{}: {val:.1}{unit}", current.0);
                } else {
                    let _ = write!(line, "{}: {val:.0}{unit}", current.0);
                }
            }
            let _ = writeln!(line, ")");
            write_line(&line);
        }
    }
}

mod dummy_debug_timer {
    pub struct DebugTimer;
    impl DebugTimer {
        #[inline(always)]
        pub fn init() -> Self {
            Self
        }
        #[inline(always)]
        pub fn init_if(_enabled: bool) -> Self {
            Self
        }
        #[inline(always)]
        pub fn record_time(&mut self, _message: &'static str) {}
        #[inline(always)]
        pub fn is_logging(&self) -> bool {
            false
        }
        #[inline(always)]
        pub fn print_times(&self, _message: &str) {}
    }
}

#[cfg(feature = "enable")]
#[macro_export]
macro_rules! debug_timer {
    ($id:ident, $($cond:tt)*) => {
        let mut $id =  {
            #[cfg($($cond)*)]
            let timer = $crate::RealDebugTimer::init();
            #[cfg(not($($cond)*))]
            let timer = $crate::DummyDebugTimer::init();
            timer
        };
    };
}

#[cfg(feature = "enable")]
#[macro_export]
macro_rules! debug_timer_type {
    ($id:ident, $($cond:tt)*) => {
        #[cfg($($cond)*)]
        pub type $id = $crate::RealDebugTimer;
        #[cfg(not($($cond)*))]
        pub type $id = $crate::DummyDebugTimer;
    };
}

#[cfg(not(feature = "enable"))]
#[macro_export]
macro_rules! debug_timer {
    ($id:ident, $($cond:tt)*) => {
        let mut $id = $crate::DummyDebugTimer::init();
    };
}

#[cfg(not(feature = "enable"))]
#[macro_export]
macro_rules! debug_timer_type {
    ($id:ident, $($cond:tt)*) => {
        pub type $id = $crate::DummyDebugTimer;
    };
}

pub use dummy_debug_timer::DebugTimer as DummyDebugTimer;
#[cfg(feature = "enable")]
pub use real_debug_timer::{is_logging, write_line as log_line, DebugTimer as RealDebugTimer};
