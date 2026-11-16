//! Per-delivery phase/command tracing.
//! - Always records phases + spawn count per thread (correct under parallel drops).
//! - `summary()` ALWAYS prints a compact phase breakdown (ms) to stderr.
//! - SHIP_TRACE=1 additionally emits JSONL events for deep analysis.
use std::cell::RefCell;
use std::sync::OnceLock;
use std::time::Instant;

thread_local! {
    static SPAWNS: RefCell<u64> = const { RefCell::new(0) };
    static PHASES: RefCell<Vec<(&'static str, f64)>> = const { RefCell::new(Vec::new()) };
}
static ENABLED: OnceLock<bool> = OnceLock::new();

pub fn enabled() -> bool {
    *ENABLED.get_or_init(|| {
        std::env::var("SHIP_TRACE")
            .map(|v| v == "1")
            .unwrap_or(false)
    })
}

pub fn count_spawn() {
    SPAWNS.with(|s| *s.borrow_mut() += 1);
}

pub fn spawns() -> u64 {
    SPAWNS.with(|s| *s.borrow())
}

pub fn reset_spawns() {
    SPAWNS.with(|s| *s.borrow_mut() = 0);
    PHASES.with(|p| p.borrow_mut().clear());
}

pub fn event(kind: &str, name: &str, ms: f64) {
    if enabled() {
        let name = name.replace('\\', "\\\\").replace('"', "\\\"");
        eprintln!("SHIP_TRACE {{\"kind\":\"{kind}\",\"name\":\"{name}\",\"ms\":{ms:.2}}}");
    }
}

pub struct Phase {
    name: &'static str,
    start: Instant,
}

pub fn phase(name: &'static str) -> Phase {
    Phase {
        name,
        start: Instant::now(),
    }
}

impl Drop for Phase {
    fn drop(&mut self) {
        let ms = self.start.elapsed().as_secs_f64() * 1000.0;
        event("phase", self.name, ms);
        PHASES.with(|p| p.borrow_mut().push((self.name, ms)));
    }
}

/// Always-on compact breakdown: `[ship-trace] prepare spawns=N fetch=.. engine=.. push=.. pr=.. merge=.. (ms)`.
pub fn summary(label: &str) {
    let line = PHASES.with(|p| {
        let p = p.borrow();
        let mut s = String::new();
        for (n, ms) in p.iter() {
            s.push_str(&format!(" {n}={ms:.0}"));
        }
        s
    });
    eprintln!("[ship-trace] {label} spawns={}{} (ms)", spawns(), line);
    if enabled() {
        eprintln!("SHIP_TRACE_SUMMARY {label} spawns={}", spawns());
    }
}
