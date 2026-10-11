use std::env;
use std::io::{self, IsTerminal, Write};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

mod catalog;
mod config;
mod net;
mod tui;

use catalog::Image;
use net::Fetched;

const NAME: &str = env!("CARGO_PKG_NAME");

const USAGE: &str = "\
linux-iso-dl - stahuje instalační obrazy linuxových distribucí

Použití:
  linux-iso-dl                 interaktivní rozhraní v terminálu
  linux-iso-dl list [text]     vypsat obrazy, případně jen ty obsahující text
  linux-iso-dl get <id>...     stáhnout obrazy do nastavené složky
  linux-iso-dl dir <složka>    nastavit složku, kam se stahuje
  linux-iso-dl show            vypsat aktuální nastavení
  linux-iso-dl check [id]...   ověřit, že obrazy z katalogu jsou k nalezení

Nastavení je v ~/.config/linux-iso-dl/config (jinou cestu určí LINUX_ISO_DL_CONFIG).";

/// How many images `check` looks up at once.
const CHECK_THREADS: usize = 8;

fn pick<'a>(images: &'a [Image], ids: &[&str]) -> Result<Vec<&'a Image>, String> {
    ids.iter()
        .map(|id| {
            images
                .iter()
                .find(|i| i.id == *id)
                .ok_or_else(|| format!("obraz {id} v katalogu není, viz {NAME} list"))
        })
        .collect()
}

fn list(filter: &[&str]) -> Result<(), String> {
    let filter = filter.join(" ");
    let images = catalog::all();
    let found: Vec<&Image> = images.iter().filter(|i| i.matches(&filter)).collect();
    if found.is_empty() {
        return Err(format!("žádný obraz neodpovídá {filter:?}"));
    }
    let id_width = found.iter().map(|i| i.id.len()).max().unwrap_or(0);
    let distro_width = found.iter().map(|i| i.distro.chars().count()).max();
    let distro_width = distro_width.unwrap_or(0);
    for image in found {
        println!(
            "{:id_width$}  {:distro_width$}  {}",
            image.id, image.distro, image.edition
        );
    }
    Ok(())
}

fn get(ids: &[&str]) -> Result<(), String> {
    let images = catalog::all();
    let wanted = pick(&images, ids)?;
    let cfg = config::load(&config::path()?)?;
    let agent = net::agent();
    let cancel = AtomicBool::new(false);
    let show_progress = io::stderr().is_terminal();
    for image in wanted {
        let url = net::resolve(&agent, image.source)?;
        let name = net::file_name(&url);
        let mut last = Instant::now();
        let mut progress = |done: u64, total: Option<u64>| {
            if !show_progress || last.elapsed() < Duration::from_millis(200) {
                return;
            }
            last = Instant::now();
            let of = total.map_or(String::new(), |t| format!(" z {}", net::human(t)));
            eprint!("\r{name}: {}{of}   ", net::human(done));
            let _ = io::stderr().flush();
        };
        let fetched = net::download(&agent, &url, &cfg.dir, &cancel, &mut progress);
        if show_progress {
            eprint!("\r\x1b[K");
        }
        let path = cfg.dir.join(name);
        match fetched? {
            Fetched::New => println!("staženo: {}", path.display()),
            Fetched::Existing => println!("už staženo: {}", path.display()),
        }
    }
    Ok(())
}

fn set_dir(dir: &str) -> Result<(), String> {
    let path = config::path()?;
    let mut cfg = config::load(&path)?;
    cfg.dir = config::expand(dir)?;
    config::save(&path, &cfg)?;
    println!("složka: {}", cfg.dir.display());
    Ok(())
}

fn show() -> Result<(), String> {
    let path = config::path()?;
    let cfg = config::load(&path)?;
    println!("nastavení: {}", path.display());
    println!("složka:    {}", cfg.dir.display());
    println!("obrazů:    {}", catalog::all().len());
    Ok(())
}

fn check(ids: &[&str]) -> Result<(), String> {
    let images = catalog::all();
    let wanted = match ids {
        [] => images.iter().collect(),
        _ => pick(&images, ids)?,
    };
    let queue = Mutex::new(wanted.iter());
    let failed = Mutex::new(0usize);
    thread::scope(|scope| {
        for _ in 0..CHECK_THREADS {
            scope.spawn(|| {
                let agent = net::agent();
                loop {
                    let Some(image) = queue.lock().unwrap().next() else {
                        return;
                    };
                    let found = net::resolve(&agent, image.source)
                        .and_then(|url| Ok((net::size(&agent, &url)?, url)));
                    match found {
                        Ok((size, url)) => {
                            let size = size.map_or("? MB".to_string(), net::human);
                            println!("ok     {:24} {size:>9}  {url}", image.id);
                        }
                        Err(e) => {
                            *failed.lock().unwrap() += 1;
                            println!("CHYBA  {:24} {e}", image.id);
                        }
                    }
                }
            });
        }
    });
    match failed.into_inner().unwrap() {
        0 => Ok(()),
        n => Err(format!("nenalezeno obrazů: {n} z {}", wanted.len())),
    }
}

fn interactive() -> Result<(), String> {
    if !io::stdout().is_terminal() {
        return Err(format!("{NAME} bez příkazu potřebuje terminál, viz --help"));
    }
    let path = config::path()?;
    let cfg = config::load(&path)?;
    tui::run(path, cfg)
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match argv.as_slice() {
        [] => interactive(),
        ["-h" | "--help" | "help"] => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        ["-V" | "--version"] => {
            println!("{NAME} {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        ["list", filter @ ..] => list(filter),
        ["get", ids @ ..] if !ids.is_empty() => get(ids),
        ["dir", dir] => set_dir(dir),
        ["show"] => show(),
        ["check", ids @ ..] => check(ids),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
