use comfy_table::{Cell, Table};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use pretty_console::{Align, Color, Line, Panel, Span, Style, Styled, pad};
use std::cell::RefCell;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

// Brand Palette
const C_PRIMARY: Color = Color::hex(0xFF5A00);
const C_NAVY: Color = Color::hex(0x1E2A38);
const C_SUCCESS: Color = Color::hex(0x10B981);
const C_ERROR: Color = Color::hex(0xEF4444);
const C_WARN: Color = Color::hex(0xF59E0B);
const C_DIM: Color = Color::hex(0x6B7280);

static LOG_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
static MULTI: OnceLock<MultiProgress> = OnceLock::new();

thread_local! {
    static PREFIX: RefCell<String> = const { RefCell::new(String::new()) };
    static ACTIVE_PB: RefCell<Option<ProgressBar>> = const { RefCell::new(None) };
}

fn mp() -> &'static MultiProgress {
    MULTI.get_or_init(MultiProgress::new)
}

pub fn init_log(path: Option<PathBuf>) {
    let _ = LOG_PATH.set(path);
}

fn append_log(plain: &str) {
    if let Some(Some(path)) = LOG_PATH.get() {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let ts = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S");
            let prefix = PREFIX.with(|c| c.borrow().clone());
            let _ = writeln!(f, "{ts} {prefix}{plain}");
        }
    }
}

fn print_line(line: &Line) {
    mp().suspend(|| line.println());
}

fn flush_buffer() {
    // No longer needed, logs print live.
}

fn print_epanel(p: &Panel) {
    mp().suspend(|| {
        eprintln!();
        eprintln!("{}", p.render());
    });
}

// ── Live Dashboard ───────────────────────────────────────────────────────

pub fn start_task(prefix: &str) {
    PREFIX.with(|c| *c.borrow_mut() = format!("[{}]", prefix));

    let pb = ProgressBar::new_spinner();
    let padded_prefix = format!("{:<12}", format!("[{}]", prefix));

    pb.set_style(
        ProgressStyle::default_spinner()
            .template(&format!(
                "{{spinner:.rgb(255,90,0)}} \x1b[90m{}\x1b[0m {{msg}}",
                padded_prefix
            ))
            .unwrap()
            .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"),
    );
    pb.enable_steady_tick(Duration::from_millis(80));

    let pb_clone = pb.clone();
    ACTIVE_PB.with(|c| *c.borrow_mut() = Some(pb_clone));
    mp().add(pb);
}

pub fn step(msg: &str) {
    ACTIVE_PB.with(|c| {
        if let Some(pb) = &*c.borrow() {
            pb.set_message(msg.to_string());
        }
    });
}

pub fn clear_task() {
    ACTIVE_PB.with(|c| {
        if let Some(pb) = c.borrow_mut().take() {
            pb.finish_and_clear();
            mp().remove(&pb);
        }
    });
}

// ── Logging Helpers ──────────────────────────────────────────────────────

fn prefix() -> String {
    PREFIX.with(|c| c.borrow().clone())
}

fn padded_prefix() -> String {
    format!("{:<12}", prefix())
}

pub fn machine(key: &str, val: &str) {
    if !std::io::stdout().is_terminal() {
        mp().suspend(|| println!("{key}={val}"));
    }
}

pub fn banner_startup() {
    if !std::io::stdout().is_terminal() {
        return;
    }
    mp().suspend(|| {
        const LOGO: &[&str] = &[
            "  ███████╗ ██╗  ██╗ ██╗ ██████╗ ",
            "  ██╔════╝ ██║  ██║ ██║ ██╔══██╗",
            "  ███████╗ ███████║ ██║ ██████╔╝",
            "  ╚════██║ ██╔══██║ ██║ ██╔═══╝ ",
            "  ███████║ ██║  ██║ ██║ ██║     ",
            "  ╚══════╝ ╚═╝  ╚═╝ ╚═╝ ╚═╝     ",
        ];

        let navy = C_NAVY;
        let orange = C_PRIMARY;
        let highlight = Color::hex(0xFFD4A8);

        println!();

        let width = 33.0;
        let frames = 40;

        for frame in 0..frames {
            if frame > 0 {
                print!("\x1B[{}A", LOGO.len());
            }

            let progress = frame as f32 / frames as f32;
            let laser_pos = progress * (width + 10.0) - 5.0;

            for line in LOGO {
                let mut spans = Vec::new();
                for (i, c) in line.chars().enumerate() {
                    if c == ' ' {
                        spans.push(Span::new(" "));
                        continue;
                    }

                    let x = i as f32;
                    let mut base_color = Color::lerp(navy, orange, x / width);

                    let dist = (x - laser_pos).abs();
                    if dist < 3.0 {
                        let intensity = 1.0 - (dist / 3.0);
                        base_color = Color::lerp(base_color, highlight, intensity);
                    }

                    spans.push(Span::new(c.to_string()).fg(base_color));
                }
                Line::from_spans(spans).println();
            }

            std::thread::sleep(Duration::from_millis(25));
        }

        println!();
        println!("          ⚓ git-only delivery pipeline");
        println!();
    });
    append_log("session start");
}

pub fn context(watch_dir: &Path, repos_root: &Path, projects: &[String], flags: &[&str]) {
    mp().suspend(|| {
        let width = 12;
        let line1 = Line::new()
            .push("  ")
            .push(pad("watching", width, Align::Left, ' '))
            .dim()
            .push("  ")
            .push(watch_dir.display().to_string());
        line1.println();
        let line2 = Line::new()
            .push("  ")
            .push(pad("repos root", width, Align::Left, ' '))
            .dim()
            .push("  ")
            .push(repos_root.display().to_string());
        line2.println();
        let count = projects.len();
        let mut line3 = Line::new()
            .push("  ")
            .push(pad("projects", width, Align::Left, ' '))
            .dim()
            .push("  ")
            .push(format!("{count}"))
            .fg(C_PRIMARY)
            .bold();
        if count > 0 {
            let sample: Vec<&str> = projects.iter().take(6).map(String::as_str).collect();
            line3 = line3
                .push("  ")
                .push("(")
                .dim()
                .push(sample.join(", "))
                .dim()
                .push("…)")
                .dim();
        }
        line3.println();
        if !flags.is_empty() {
            let line4 = Line::new()
                .push("  ")
                .push(pad("flags", width, Align::Left, ' '))
                .dim()
                .push("  ")
                .push(flags.join("  ·  "));
            line4.println();
        }
        println!();
    });
    append_log(&format!(
        "watching={} repos={} projects={}",
        watch_dir.display(),
        repos_root.display(),
        projects.len()
    ));
}

pub fn ready() {
    mp().suspend(|| {
        println!(
            "  {}  drop a {{project}}-{{slug}}.zip to ship.\n",
            Span::new("ready.").fg(C_SUCCESS).bold()
        )
    });
    append_log("ready");
}

pub fn projects_table(root: &Path, projects: &[String]) {
    mp().suspend(|| {
        println!("\n  {}  {}", "repos_root".dim(), root.display());
        println!("  {}  {}\n", "projects".dim(), projects.len());
        if projects.is_empty() {
            return;
        }
        let mut table = Table::new();
        table.set_header(vec!["Project", "Project", "Project", "Project"]);
        let cols = 4;
        let rows = projects.len().div_ceil(cols);
        for r in 0..rows {
            let mut row = Vec::new();
            for c in 0..cols {
                let i = r + c * rows;
                row.push(Cell::new(projects.get(i).map(|s| s.as_str()).unwrap_or("")));
            }
            table.add_row(row);
        }
        println!("{table}\n");
    });
}

pub fn section_begin(zip_name: &str, detail: Option<&str>) {
    let line = Line::new()
        .push(padded_prefix())
        .fg(C_DIM)
        .push("📦 ")
        .fg(C_PRIMARY)
        .push(zip_name)
        .fg(C_PRIMARY)
        .bold();
    print_line(&line);
    if let Some(d) = detail {
        let meta = Line::new()
            .push(" ".repeat(13))
            .push("└─ ")
            .dim()
            .push(d)
            .dim();
        print_line(&meta);
    }
    append_log(&format!("BEGIN {zip_name}"));
}

pub fn section_ok(project: &str, pr_url: Option<&str>, elapsed: Duration) {
    clear_task();
    let time = human_duration(elapsed);
    flush_buffer();

    let mut p = Panel::new()
        .title(format!("✅ Shipped Successfully: {}", project))
        .border(Style::new().fg(C_SUCCESS))
        .width(74)
        .padding(2);

    p = p.line(Line::new().push("📂 Project  ").dim().push(project).white());
    if let Some(url) = pr_url {
        p = p.line(
            Line::new()
                .push("🔗 PR       ")
                .dim()
                .push(url)
                .fg(C_PRIMARY)
                .bold(),
        );
    }
    p = p.line(
        Line::new()
            .push("⏱ Time      ")
            .dim()
            .push(time.as_str())
            .white(),
    );

    mp().suspend(|| {
        println!();
        p.println();
    });
    append_log(&format!("[OK] {project} merged ({})", time));
}

pub fn commit_step(idx: usize, total: usize, subject: &str) {
    let counter = format!("[{:>2}/{}]", idx, total);

    let (p_str, rest) = if let Some(pos) = subject.find(':') {
        let (p, r) = subject.split_at(pos);
        (p.trim(), r[1..].trim())
    } else {
        ("", subject)
    };

    let colored_prefix = if p_str.starts_with("feat") {
        Span::new(p_str).fg(C_SUCCESS)
    } else if p_str.starts_with("fix") {
        Span::new(p_str).fg(C_ERROR)
    } else if p_str.starts_with("docs") {
        Span::new(p_str).fg(C_PRIMARY)
    } else {
        Span::new(p_str).dim()
    };

    let mut line = Line::new()
        .push(padded_prefix())
        .fg(C_DIM)
        .push("✓ ")
        .fg(C_SUCCESS)
        .dim()
        .push(counter.as_str())
        .fg(C_DIM)
        .push("  ");

    if !p_str.is_empty() {
        if let Some(scope_end) = rest.find(')') {
            let (scope, msg) = rest.split_at(scope_end + 1);
            line = line
                .push(colored_prefix)
                .push(scope)
                .dim()
                .push(msg)
                .white();
        } else {
            line = line
                .push(colored_prefix)
                .push(": ")
                .white()
                .push(rest)
                .white();
        }
    } else {
        line = line.push(rest).white();
    }
    print_line(&line);
    step(&format!("committed {}", counter));
}

pub fn version_step(from: &str, to: &str) {
    let line = Line::new()
        .push(padded_prefix())
        .fg(C_DIM)
        .push("✓ ")
        .fg(C_SUCCESS)
        .dim()
        .push(pad("version", 10, Align::Left, ' '))
        .dim()
        .push(from)
        .white()
        .push(" → ")
        .dim()
        .push(to)
        .fg(C_PRIMARY)
        .bold();
    print_line(&line);
}

pub fn version_skipped(at: &str, reason: &str) {
    let line = Line::new()
        .push(padded_prefix())
        .fg(C_DIM)
        .push("⏭ ")
        .fg(C_WARN)
        .dim()
        .push(pad("version", 10, Align::Left, ' '))
        .dim()
        .push(at)
        .white()
        .push("  ")
        .push(format!("({})", reason))
        .dim();
    print_line(&line);
}

pub fn pr_opened(url: &str) {
    let line = Line::new()
        .push(padded_prefix())
        .fg(C_DIM)
        .push("✓ ")
        .fg(C_SUCCESS)
        .dim()
        .push(pad("PR", 10, Align::Left, ' '))
        .dim()
        .push("↗ ")
        .fg(C_PRIMARY)
        .push(Span::new(url).fg(C_PRIMARY).underline().link(url));
    print_line(&line);
    machine("PR_URL", url);
}

pub fn pr_reused(url: &str) {
    let line = Line::new()
        .push(padded_prefix())
        .fg(C_DIM)
        .push("✓ ")
        .fg(C_SUCCESS)
        .dim()
        .push(pad("PR", 10, Align::Left, ' '))
        .dim()
        .push("(reused) ")
        .dim()
        .push("↗ ")
        .fg(C_PRIMARY)
        .push(Span::new(url).fg(C_PRIMARY).underline().link(url));
    print_line(&line);
    machine("PR_URL", url);
}

pub fn merged(sha: &str) {
    let line = Line::new()
        .push(padded_prefix())
        .fg(C_DIM)
        .push("✓ ")
        .fg(C_SUCCESS)
        .dim()
        .push(pad("merged", 10, Align::Left, ' '))
        .dim()
        .push(sha)
        .white()
        .push(" (rebase)")
        .dim();
    print_line(&line);
    machine("MERGED", sha);
}

pub fn warn(msg: &str) {
    let line = Line::new()
        .push(padded_prefix())
        .fg(C_DIM)
        .push("⚠ ")
        .fg(C_WARN)
        .push(msg)
        .fg(C_WARN);
    mp().suspend(|| eprintln!("{}", line.render()));
}

pub fn skip(msg: &str) {
    let line = Line::new()
        .push(padded_prefix())
        .fg(C_DIM)
        .push("↷ ")
        .dim()
        .push(msg)
        .dim();
    mp().suspend(|| println!("{}", line.render()));
}

pub fn info(msg: &str) {
    let line = Line::new()
        .push(padded_prefix())
        .fg(C_DIM)
        .push("  · ")
        .dim()
        .push(msg)
        .white();
    mp().suspend(|| println!("{}", line.render()));
}

pub fn resume() {
    let line = Line::new()
        .push(padded_prefix())
        .fg(C_DIM)
        .push("⏭ ")
        .fg(C_WARN)
        .dim()
        .push("resume")
        .dim()
        .push("  tree clean, all groups already committed")
        .white();
    print_line(&line);
}

pub fn error_panel(headline: &str, detail: &str, recovery: &[String]) {
    clear_task();
    flush_buffer();
    let hl = Line::new()
        .push(padded_prefix())
        .fg(C_DIM)
        .push("✗ ")
        .fg(C_ERROR)
        .bold()
        .push("ship failed")
        .fg(C_ERROR)
        .bold()
        .push("   ")
        .push(headline)
        .white();
    mp().suspend(|| {
        eprintln!();
        eprintln!("{}", hl.render());
    });
    if !detail.trim().is_empty() {
        let mut p = Panel::new()
            .title("why")
            .border(Style::new().fg(C_DIM))
            .width(74)
            .soft_wrap(true);
        for l in detail.lines() {
            p = p.line(l.to_string());
        }
        print_epanel(&p);
    }
    if !recovery.is_empty() {
        let mut p = Panel::new()
            .title("fix")
            .border(Style::new().fg(C_PRIMARY))
            .width(74)
            .soft_wrap(true);
        for cmd in recovery {
            p = p.line(
                Line::new()
                    .push("  ")
                    .push("$ ")
                    .fg(C_PRIMARY)
                    .push(cmd.clone())
                    .white(),
            );
        }
        print_epanel(&p);
    }
}

pub fn plan_header(branch: &str, new: &str, first_release: bool) {
    mp().suspend(|| {
        println!(
            "
  ──────────────────────────────────────────────────────────────────"
        );
        Line::new()
            .push("  📦 ")
            .push("ship plan")
            .fg(C_PRIMARY)
            .bold()
            .push(" (dry-run)")
            .dim()
            .println();
        println!();
        let width = 10;
        Line::new()
            .push("  ")
            .push(pad("branch", width, Align::Left, ' '))
            .dim()
            .push("  ")
            .push(branch)
            .println();
        if first_release {
            Line::new()
                .push("  ")
                .push(pad("release", width, Align::Left, ' '))
                .dim()
                .push("  ")
                .push("first release")
                .println();
        } else {
            Line::new()
                .push("  ")
                .push(pad("version", width, Align::Left, ' '))
                .dim()
                .push("  ")
                .push(format!("→ {new}"))
                .println();
        }
        println!();
    });
}

pub fn plan_commit(_n: usize, _count: usize, _msg: &str) {}
pub fn plan_footer() {
    mp().suspend(|| println!("\n  run `ship prepare` to ship."));
}

pub fn final_success(pr_url: Option<&str>, merged: Option<&str>) {
    clear_task();
    flush_buffer();
    let mut p = Panel::new()
        .title("✅ Shipped Successfully")
        .border(Style::new().fg(C_SUCCESS))
        .width(74)
        .padding(2);
    if let Some(pr) = pr_url {
        p = p.line(
            Line::new()
                .push("PR      ")
                .dim()
                .push("↗ ")
                .fg(C_PRIMARY)
                .push(pr)
                .white(),
        );
    }
    if let Some(m) = merged {
        p = p.line(Line::new().push("Merged  ").dim().push(m).white());
    }
    mp().suspend(|| {
        println!();
        p.println();
    });
}

pub fn final_summary(shipped: usize, failed: usize, elapsed: Duration) {
    mp().suspend(|| {
        println!("\n  ──────────────────────────────────────────────────────────────────");
        let mut line = Line::new().push("  ");
        if failed > 0 {
            line = line
                .push("✗")
                .fg(C_ERROR)
                .bold()
                .push(format!(" {} failed", failed))
                .fg(C_ERROR)
                .bold();
            if shipped > 0 {
                line = line
                    .push("  ·  ")
                    .dim()
                    .push(format!("{} shipped", shipped))
                    .fg(C_SUCCESS);
            }
        } else if shipped > 0 {
            line = line
                .push("✨")
                .fg(C_SUCCESS)
                .bold()
                .push(format!(" Shipped {} zip(s)", shipped))
                .fg(C_SUCCESS)
                .bold();
        } else {
            line = line.push("Nothing to ship").dim();
        }
        line = line
            .push("  ·  ")
            .dim()
            .push("⏱")
            .dim()
            .push(" ")
            .push(human_duration(elapsed))
            .dim();
        line.println();
        println!();
    });
}

fn human_duration(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    }
}
