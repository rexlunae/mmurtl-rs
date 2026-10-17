//! MMURTL-style CLI. Command behavior is based on MSAMPLES/CLI/CLI.C in
//! https://github.com/bproctor/MMURTL (Richard A. Burgess, public domain).
//! This first port is a kernel task using the existing exFAT and ELF APIs.

mod parse;

use crate::fs;
use crate::{scheduler, serial};
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, Ordering};
use parse::{Command, Path, MAX_LINE};

static ACTIVE: AtomicBool = AtomicBool::new(false);
// The CLI is the only caller. Keep its bounded PS snapshot off-stack and
// reuse it instead of leaking one heap allocation per command.
static PS_SNAPSHOT: spin::Mutex<[(u32, &'static str, scheduler::TaskState, bool);
    scheduler::MAX_TASKS + scheduler::MAX_CPUS]> = spin::Mutex::new(
        [(0, "", scheduler::TaskState::Exited, false); scheduler::MAX_TASKS + scheduler::MAX_CPUS]);
pub fn active() -> bool {
    ACTIVE.load(Ordering::Acquire)
}

pub fn start() {
    scheduler::create_task(shell_task, scheduler::PRIORITY_DEFAULT, "cli");
}

macro_rules! out {
    ($($arg:tt)*) => {{
        let mut line: heapless::String<512> = heapless::String::new();
        let _ = write!(line, $($arg)*);
        serial::write_str(&line);
    }};
}

extern "C" fn shell_task() -> ! {
    while !crate::userspace::checks_finished() {
        scheduler::sleep_ms(20);
    }
    ACTIVE.store(true, Ordering::Release);
    // Workers see ACTIVE and exit on their next wakeup. Wait for them so
    // their final output cannot interrupt the first prompt.
    scheduler::sleep_ms(1100);
    serial::write_str("\nMMURTL/RS CLI — type HELP for commands.\nConsole: serial terminal (amd64 also accepts the PS/2 keyboard).\n");
    let mut cwd = Path::root();
    let mut line: heapless::String<MAX_LINE> = heapless::String::new();
    let mut overflow = false;
    let mut was_cr = false;
    let mut utf8 = [0u8; 4];
    let mut pending = 0usize;
    let mut invalid = false;
    prompt(&cwd);
    loop {
        crate::arch::serial::poll_input();
        let c = match crate::keyboard::pop_input() {
            Some(crate::keyboard::InputEvent::Byte(c)) => c,
            Some(crate::keyboard::InputEvent::Lost) => {
                line.clear();
                overflow = false;
                invalid = false;
                pending = 0;
                was_cr = false;
                serial::write_str(
                    "\nConsole input lost; queued input discarded through the next Enter.\n",
                );
                prompt(&cwd);
                continue;
            }
            None => {
                scheduler::sleep_ms(10);
                continue;
            }
        };
        if c >= 0x80 && !overflow {
            was_cr = false;
            utf8[pending] = c;
            pending += 1;
            match core::str::from_utf8(&utf8[..pending]) {
                Ok(text) => {
                    if line.push_str(text).is_err() {
                        overflow = true;
                    } else {
                        serial::write_str(text);
                    }
                    pending = 0;
                }
                Err(e) if e.error_len().is_none() && pending < 4 => {}
                Err(_) => {
                    invalid = true;
                    pending = 0;
                }
            }
            continue;
        }
        if pending != 0 {
            invalid = true;
            pending = 0;
        }
        if c == b'\n' && was_cr {
            was_cr = false;
            continue;
        }
        was_cr = c == b'\r';
        match c {
            b'\r' | b'\n' => {
                serial::write_str("\n");
                if invalid {
                    serial::write_line("Error: invalid UTF-8 input; discarded");
                } else if overflow {
                    serial::write_line("Error: command too long; discarded");
                } else if let Err(e) = execute(&line, &mut cwd) {
                    out!("Error: {}\n", e);
                }
                line.clear();
                overflow = false;
                invalid = false;
                prompt(&cwd);
            }
            3 | 21 => {
                // Ctrl-C / Ctrl-U: cancel input, not a running program
                line.clear();
                overflow = false;
                invalid = false;
                serial::write_str("^C\n");
                prompt(&cwd);
            }
            8 | 127 if !overflow => {
                if line.pop().is_some() {
                    serial::write_str("\x08 \x08");
                }
            }
            b' '..=b'~' if !overflow => {
                if line.push(c as char).is_err() {
                    overflow = true;
                } else {
                    serial::write_str(core::str::from_utf8(&[c]).unwrap());
                }
            }
            _ => {}
        }
    }
}

fn prompt(cwd: &Path) {
    out!("{}> ", cwd.as_str());
}

#[inline(never)]
fn execute(line: &str, cwd: &mut Path) -> Result<(), &'static str> {
    let cmd = Command::parse(line)?;
    if cmd.len() == 0 {
        return Ok(());
    }
    let name = cmd.arg(0);
    let mut upper: heapless::String<MAX_LINE> = heapless::String::new();
    for c in name.chars() {
        let _ = upper.push(c.to_ascii_uppercase());
    }
    let count = cmd.len() - 1;
    let one = || {
        if count == 1 {
            Ok(cmd.arg(1))
        } else {
            Err("expected one argument (see HELP)")
        }
    };
    let two = || {
        if count == 2 {
            Ok((cmd.arg(1), cmd.arg(2)))
        } else {
            Err("expected two arguments (see HELP)")
        }
    };
    match upper.as_str() {
        "HELP" if count == 0 => serial::write_str(
            "Commands (case insensitive; quote paths containing spaces):\n\
             HELP                 Show commands\n\
             CLS                  Clear the serial terminal (ANSI)\n\
             DIR [path]           List directory (LS)\n\
             PATH [path]          Show/set current directory (CD, PWD)\n\
             TYPE file            Display text file (CAT)\n\
             DUMP file            Hex dump file\n\
             COPY source dest     Copy a file; destination must not exist\n\
             DEL file             Delete a file (RM)\n\
             MD path / RD path    Create/remove empty directory (MKDIR/RMDIR)\n\
             WRITE file text      Create/replace a text file\n\
             RUN file             Run ELF program and wait (also: program name)\n\
             VER / MEM / PS       Version, free memory, tasks\n\
             UPTIME / ECHO text    Uptime, print text\n\
             Backspace edits; Ctrl-U or Ctrl-C cancels the input line.\n\
             One FAT or exFAT volume at /; RUN searches current directory then /BIN.\n",
        ),
        "CLS" if count == 0 => serial::write_str("\x1b[2J\x1b[H"),
        "DIR" | "LS" if count <= 1 => {
            let path = cwd.resolve(if count == 0 { "." } else { cmd.arg(1) })?;
            let entries = fs::read_dir(path.as_str())
                .ok_or("directory unavailable (attach a FAT or exFAT disk)")?;
            out!("Directory {}\n", path.as_str());
            for (name, dir, size) in entries {
                if dir {
                    out!("  <DIR>  {}\n", name);
                } else {
                    out!("  {:>8}  {}\n", size, name);
                }
            }
        }
        "PATH" | "CD" | "PWD" if count <= 1 => {
            if count == 0 {
                out!("{}\n", cwd.as_str());
            } else {
                let path = cwd.resolve(cmd.arg(1))?;
                if path.as_str() != "/" && fs::read_dir(path.as_str()).is_none() {
                    return Err("no such directory");
                }
                *cwd = path;
            }
        }
        "TYPE" | "CAT" | "DUMP" => {
            let path = cwd.resolve(one()?)?;
            let bytes = read_file(&path)?;
            if upper.as_str() == "DUMP" {
                for (i, row) in bytes.chunks(16).enumerate() {
                    let mut text: heapless::String<128> = heapless::String::new();
                    let _ = write!(text, "{:08x}  ", i * 16);
                    for b in row {
                        let _ = write!(text, "{:02x} ", b);
                    }
                    let _ = text.push_str(" |");
                    for b in row {
                        let _ = text.push(if b.is_ascii_graphic() || *b == b' ' {
                            *b as char
                        } else {
                            '.'
                        });
                    }
                    let _ = text.push_str("|\n");
                    serial::write_str(&text);
                }
            } else {
                // Prevent files from injecting terminal control sequences.
                for row in bytes.chunks(128) {
                    let mut text: heapless::String<256> = heapless::String::new();
                    for b in row {
                        let _ = text.push(
                            if b.is_ascii_graphic() || matches!(*b, b' ' | b'\n' | b'\t') {
                                *b as char
                            } else if *b == b'\r' {
                                continue;
                            } else {
                                '.'
                            },
                        );
                    }
                    serial::write_str(&text);
                }
                serial::write_str("\n");
            }
        }
        "COPY" => {
            let (from, to) = two()?;
            let bytes = read_file(&cwd.resolve(from)?)?;
            fs::create_file(cwd.resolve(to)?.as_str(), &bytes)?;
        }
        "WRITE" => {
            let (path, text) = two()?;
            fs::write_file(cwd.resolve(path)?.as_str(), text.as_bytes())?;
        }
        "MD" | "MKDIR" => fs::mkdir(cwd.resolve(one()?)?.as_str())?,
        "DEL" | "RM" | "RD" | "RMDIR" => {
            let path = cwd.resolve(one()?)?;
            let is_dir = fs::read_dir(path.as_str()).is_some();
            let want_dir = matches!(upper.as_str(), "RD" | "RMDIR");
            let same_dir = matches!((fs::directory_id(path.as_str()), fs::directory_id(cwd.as_str())),
                (Some(a), Some(b)) if a == b);
            if want_dir && (same_dir || path.as_str() == "/") {
                return Err("cannot remove the current or root directory");
            }
            if is_dir != want_dir {
                return Err(if is_dir {
                    "use RD to remove a directory"
                } else {
                    "use DEL to remove a file"
                });
            }
            fs::delete(path.as_str())?;
        }
        "RUN" => run(cwd, one()?)?,
        "VER" if count == 0 => out!(
            "{} v{} ({})\n",
            crate::KERNEL_NAME,
            crate::VERSION,
            crate::arch::NAME
        ),
        "MEM" if count == 0 => {
            out!("{} KiB physical memory free\n",crate::memory::heap::free_frames()*4);
            let (live,cached,objects)=crate::memory::heap::stats();
            out!("{} KiB heap live, {} KiB slab backing, {} live allocations\n",live/1024,cached/1024,objects);
        }
        "UPTIME" if count == 0 => out!(
            "{} ms\n",
            scheduler::jiffies() * 1000 / scheduler::SCHEDULER_FREQUENCY_HZ as u64
        ),
        "PS" if count == 0 => {
            let mut tasks = PS_SNAPSHOT.lock();
            let (count, truncated) = scheduler::task_snapshot(&mut *tasks);
            for &(id, name, state, user) in &tasks[..count] {
                out!(
                    "{:>4} {:<16} {:?} {}\n",
                    id,
                    name,
                    state,
                    if user { "user" } else { "kernel" }
                );
            }
            if truncated {
                serial::write_line("(more tasks than fit in this snapshot)");
            }
        }
        "ECHO" => {
            for i in 1..cmd.len() {
                if i > 1 {
                    serial::write_str(" ");
                }
                serial::write_str(cmd.arg(i));
            }
            serial::write_str("\n");
        }
        "REN" | "RENAME" | "DEBUG" | "MONITOR" | "EXIT" => {
            return Err("this original CLI command is not yet supported")
        }
        "HELP" | "CLS" | "DIR" | "LS" | "PATH" | "CD" | "PWD" | "VER" | "MEM" | "UPTIME" | "PS" => {
            return Err("wrong number of arguments (see HELP)")
        }
        _ if count == 0 => run(cwd, name)?,
        _ => return Err("unknown command or unsupported program arguments (see HELP)"),
    }
    Ok(())
}

// The kernel heap currently does not reclaim allocations. Bound individual
// file reads until streaming file APIs and a reclaiming allocator exist.
fn read_file(path: &Path) -> Result<alloc::vec::Vec<u8>, &'static str> {
    const LIMIT: u64 = 1024 * 1024;
    let (dir, size) = fs::metadata(path.as_str()).ok_or("file not found")?;
    if dir {
        return Err("is a directory");
    }
    if size > LIMIT {
        return Err("file exceeds the shell's 1 MiB read limit");
    }
    fs::read_file(path.as_str()).ok_or("file read failed")
}

fn run(cwd: &Path, name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        return Err("missing program name");
    }
    let mut file: heapless::String<MAX_LINE> = heapless::String::new();
    file.push_str(name).map_err(|_| "path too long")?;
    if !name.rsplit(['/', '\\']).next().unwrap_or("").contains('.') {
        file.push_str(".ELF").map_err(|_| "path too long")?;
    }
    let local = cwd.resolve(&file)?;
    let bytes = match read_file(&local) {
        Ok(bytes) => bytes,
        Err("file not found") if !name.contains(['/', '\\']) => {
            read_file(&Path::root().resolve("BIN")?.resolve(&file)?)?
        }
        Err(e) => return Err(e),
    };
    let image = crate::memory::user::load_elf(&bytes)?;
    // A static name avoids leaking one task-name allocation per invocation.
    let tid = scheduler::create_user_task(
        image.entry,
        image.stack_top,
        image.startup,
        image.space,
        image.slot,
        "shell-program",
    );
    out!("Running {} as T{}\n", name, tid);
    while !matches!(
        scheduler::task_state(tid),
        Some(scheduler::TaskState::Exited) | None
    ) {
        scheduler::sleep_ms(20);
    }
    if crate::userspace::was_killed(tid) {
        return Err("program faulted");
    }
    Ok(())
}
