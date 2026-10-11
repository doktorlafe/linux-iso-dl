//! The interactive interface: the catalog with checkboxes and the downloads.

use std::io;
use std::mem;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Gauge, List, ListState, Paragraph};
use ratatui::{DefaultTerminal, Frame};

use crate::catalog::{self, Image};
use crate::config::{self, Config};
use crate::net::{self, Fetched};
use crate::NAME;

/// How often the screen is redrawn while waiting for a key.
const TICK: Duration = Duration::from_millis(150);

enum Mode {
    Main,
    /// Typing the filter; the list narrows with every key.
    Filter,
    /// Typing a new download directory on the status line.
    Dir(String),
}

enum State {
    Idle,
    Queued,
    Resolving,
    Downloading { done: u64, total: Option<u64> },
    Done(&'static str),
    Failed(String),
}

/// What the download thread reports, with the index of the image.
enum Msg {
    Resolving(usize),
    Started(usize, String),
    Progress(usize, u64, Option<u64>),
    Finished(usize, Result<Fetched, String>),
}

/// The file being downloaded right now, for the progress bar.
struct Current {
    name: String,
    /// Start of the interval the speed is measured over, and bytes done then.
    since: Instant,
    base: u64,
    /// Bytes per second.
    speed: f64,
}

/// A line of the list.
#[derive(Clone, Copy, PartialEq)]
enum Row {
    /// The header of a distribution with several images.
    Group(&'static str),
    Image(usize),
}

struct App {
    path: PathBuf,
    cfg: Config,
    images: Vec<Image>,
    checked: Vec<bool>,
    states: Vec<State>,
    filter: String,
    /// What the list shows: groups, and the images of the open ones.
    rows: Vec<Row>,
    /// Distributions whose group is open.
    open: Vec<&'static str>,
    list: ListState,
    mode: Mode,
    /// Message for the status line and whether it is an error.
    status: Option<(String, bool)>,
    /// Open while the download thread runs.
    rx: Option<Receiver<Msg>>,
    cancel: Arc<AtomicBool>,
    current: Option<Current>,
    /// Set by the first `q` during a download; the second one quits.
    quitting: bool,
}

fn clock(secs: f64) -> String {
    let secs = secs as u64;
    if secs >= 3600 {
        format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
    } else {
        format!("{}:{:02}", secs / 60, secs % 60)
    }
}

/// What the list says about an image, and in which colour.
fn state_text(state: &State) -> (String, Style) {
    let dim = Style::new().fg(Color::DarkGray);
    match state {
        State::Idle => (String::new(), dim),
        State::Queued => ("ve frontě".to_string(), dim),
        State::Resolving => ("hledám…".to_string(), Style::new().fg(Color::Cyan)),
        State::Downloading { done, total } => {
            let text = match total {
                Some(total) if *total > 0 => format!("{} %", done * 100 / total),
                _ => net::human(*done),
            };
            (text, Style::new().fg(Color::Cyan))
        }
        State::Done(how) => (how.to_string(), Style::new().fg(Color::Green)),
        State::Failed(_) => ("chyba".to_string(), Style::new().fg(Color::Red)),
    }
}

impl App {
    fn sel(&self) -> usize {
        self.list.selected().unwrap_or(0)
    }

    fn select(&mut self, pos: usize) {
        let last = self.rows.len().saturating_sub(1);
        self.list.select(Some(pos.min(last)));
    }

    /// The images that pass the filter.
    fn matching(&self) -> Vec<usize> {
        (0..self.images.len())
            .filter(|&i| self.images[i].matches(&self.filter))
            .collect()
    }

    /// The images of a distribution that pass the filter.
    fn members(&self, distro: &str) -> Vec<usize> {
        let mut found = self.matching();
        found.retain(|&i| self.images[i].distro == distro);
        found
    }

    /// A distribution with a single image is listed directly, without a group.
    fn is_group(&self, distro: &str) -> bool {
        self.images.iter().filter(|i| i.distro == distro).count() > 1
    }

    /// While filtering, every group shows what matched in it.
    fn is_open(&self, distro: &str) -> bool {
        !self.filter.is_empty() || self.open.contains(&distro)
    }

    fn rebuild(&mut self) {
        let mut rows = Vec::new();
        let mut seen = Vec::new();
        for image in &self.images {
            let distro = image.distro;
            if seen.contains(&distro) {
                continue;
            }
            seen.push(distro);
            let members = self.members(distro);
            if members.is_empty() {
                continue;
            }
            if self.is_group(distro) {
                rows.push(Row::Group(distro));
                if !self.is_open(distro) {
                    continue;
                }
            }
            rows.extend(members.into_iter().map(Row::Image));
        }
        self.rows = rows;
    }

    fn refilter(&mut self) {
        self.rebuild();
        *self.list.offset_mut() = 0;
        self.select(0);
    }

    /// Opens or closes the group the selected row belongs to (`None` flips
    /// it) and leaves the selection on its header.
    fn set_open(&mut self, open: Option<bool>) {
        let distro = match self.rows.get(self.sel()) {
            Some(Row::Group(distro)) => *distro,
            Some(Row::Image(i)) => self.images[*i].distro,
            None => return,
        };
        if !self.is_group(distro) || !self.filter.is_empty() {
            return;
        }
        let was_open = self.open.contains(&distro);
        self.open.retain(|d| *d != distro);
        if open.unwrap_or(!was_open) {
            self.open.push(distro);
        }
        self.rebuild();
        let header = self.rows.iter().position(|r| *r == Row::Group(distro));
        self.select(header.unwrap_or(0));
    }

    fn toggle(&mut self) {
        match self.rows.get(self.sel()) {
            Some(Row::Image(i)) => {
                self.checked[*i] = !self.checked[*i];
                self.select(self.sel() + 1);
            }
            // On a header: the whole group, or none of it if all was checked.
            Some(Row::Group(distro)) => {
                let members = self.members(distro);
                let all = members.iter().all(|&i| self.checked[i]);
                for i in members {
                    self.checked[i] = !all;
                }
            }
            None => {}
        }
    }

    /// Checks everything that passes the filter, or unchecks it if it all is checked.
    fn toggle_all(&mut self) {
        let matching = self.matching();
        let all = matching.iter().all(|&i| self.checked[i]);
        for i in matching {
            self.checked[i] = !all;
        }
    }

    fn set_dir(&mut self, text: &str) {
        let dir = match config::expand(text) {
            Ok(dir) => dir,
            Err(e) => return self.status = Some((e, true)),
        };
        self.cfg.dir = dir;
        self.status = Some(match config::save(&self.path, &self.cfg) {
            Ok(()) => (format!("složka: {}", self.cfg.dir.display()), false),
            Err(e) => (e, true),
        });
    }

    fn start(&mut self) {
        if self.rx.is_some() {
            self.status = Some(("stahování už běží, x ho zruší".to_string(), true));
            return;
        }
        let jobs: Vec<(usize, &'static str)> = (0..self.images.len())
            .filter(|&i| self.checked[i])
            .map(|i| (i, self.images[i].source))
            .collect();
        if jobs.is_empty() {
            let hint = "nic není vybráno, mezerník obraz zaškrtne a d stáhne";
            self.status = Some((hint.to_string(), true));
            return;
        }
        for (i, _) in &jobs {
            self.states[*i] = State::Queued;
        }
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = Arc::clone(&self.cancel);
        let dir = self.cfg.dir.clone();
        thread::spawn(move || {
            let agent = net::agent();
            for (i, source) in jobs {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                let _ = tx.send(Msg::Resolving(i));
                let result = net::resolve(&agent, source).and_then(|url| {
                    let _ = tx.send(Msg::Started(i, net::file_name(&url).to_string()));
                    let mut last = Instant::now();
                    net::download(&agent, &url, &dir, &cancel, &mut |done, total| {
                        if last.elapsed() >= TICK {
                            last = Instant::now();
                            let _ = tx.send(Msg::Progress(i, done, total));
                        }
                    })
                });
                let _ = tx.send(Msg::Finished(i, result));
            }
        });
    }

    /// Takes in what the download thread reported since the last look.
    fn drain(&mut self) {
        loop {
            let msg = match self.rx.as_ref().map(Receiver::try_recv) {
                Some(Ok(msg)) => msg,
                Some(Err(TryRecvError::Empty)) | None => return,
                Some(Err(TryRecvError::Disconnected)) => return self.finish(),
            };
            match msg {
                Msg::Resolving(i) => self.states[i] = State::Resolving,
                Msg::Started(i, name) => {
                    self.states[i] = State::Downloading {
                        done: 0,
                        total: None,
                    };
                    self.current = Some(Current {
                        name,
                        since: Instant::now(),
                        base: 0,
                        speed: 0.0,
                    });
                }
                Msg::Progress(i, done, total) => {
                    self.states[i] = State::Downloading { done, total };
                    if let Some(cur) = &mut self.current {
                        let elapsed = cur.since.elapsed().as_secs_f64();
                        if cur.base == 0 {
                            // A continued download starts well above zero.
                            cur.base = done;
                            cur.since = Instant::now();
                        } else if elapsed >= 1.0 {
                            cur.speed = done.saturating_sub(cur.base) as f64 / elapsed;
                            cur.base = done;
                            cur.since = Instant::now();
                        }
                    }
                }
                Msg::Finished(i, result) => {
                    self.current = None;
                    self.states[i] = match result {
                        Ok(fetched) => {
                            self.checked[i] = false;
                            State::Done(match fetched {
                                Fetched::New => "staženo",
                                Fetched::Existing => "už staženo",
                            })
                        }
                        // Cancelled, not failed: it stays checked for the next round.
                        Err(_) if self.cancel.load(Ordering::Relaxed) => State::Idle,
                        Err(e) => State::Failed(e),
                    };
                }
            }
        }
    }

    /// The download thread ended: sums up how it went.
    fn finish(&mut self) {
        self.rx = None;
        self.current = None;
        self.quitting = false;
        let mut failed = 0;
        for state in &mut self.states {
            match state {
                State::Failed(_) => failed += 1,
                State::Queued | State::Resolving => *state = State::Idle,
                _ => {}
            }
        }
        self.status = Some(if self.cancel.load(Ordering::Relaxed) {
            ("stahování zrušeno, příště naváže".to_string(), false)
        } else if failed > 0 {
            (format!("hotovo, selhalo: {failed}"), true)
        } else {
            (
                format!("hotovo, uloženo v {}", self.cfg.dir.display()),
                false,
            )
        });
    }

    /// Returns true when the program should quit.
    fn on_key(&mut self, code: KeyCode, mods: KeyModifiers) -> bool {
        if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
            return true;
        }
        self.status = None;
        let quitting = mem::take(&mut self.quitting);
        match &mut self.mode {
            Mode::Filter => match code {
                KeyCode::Enter => self.mode = Mode::Main,
                KeyCode::Esc => {
                    self.filter.clear();
                    self.mode = Mode::Main;
                    self.refilter();
                }
                KeyCode::Backspace => {
                    self.filter.pop();
                    self.refilter();
                }
                KeyCode::Char(c) => {
                    self.filter.push(c);
                    self.refilter();
                }
                _ => {}
            },
            Mode::Dir(text) => match code {
                KeyCode::Enter => {
                    let text = mem::take(text);
                    self.mode = Mode::Main;
                    self.set_dir(&text);
                }
                KeyCode::Esc => self.mode = Mode::Main,
                KeyCode::Backspace => {
                    text.pop();
                }
                KeyCode::Char(c) => text.push(c),
                _ => {}
            },
            Mode::Main => match code {
                KeyCode::Char('q') if self.rx.is_some() && !quitting => {
                    self.quitting = true;
                    let warn = "stahování běží, další q ho přeruší a ukončí program";
                    self.status = Some((warn.to_string(), true));
                }
                KeyCode::Char('q') => return true,
                KeyCode::Esc if !self.filter.is_empty() => {
                    self.filter.clear();
                    self.refilter();
                }
                KeyCode::Up | KeyCode::Char('k') => self.select(self.sel().saturating_sub(1)),
                KeyCode::Down | KeyCode::Char('j') => self.select(self.sel() + 1),
                KeyCode::PageUp => self.select(self.sel().saturating_sub(10)),
                KeyCode::PageDown => self.select(self.sel() + 10),
                KeyCode::Home | KeyCode::Char('g') => self.select(0),
                KeyCode::End | KeyCode::Char('G') => self.select(usize::MAX),
                KeyCode::Char(' ') => self.toggle(),
                KeyCode::Char('a') => self.toggle_all(),
                KeyCode::Char('/') => self.mode = Mode::Filter,
                KeyCode::Char('o') if self.rx.is_some() => {
                    let msg = "složku nelze měnit během stahování";
                    self.status = Some((msg.to_string(), true));
                }
                KeyCode::Char('o') => self.mode = Mode::Dir(self.cfg.dir.display().to_string()),
                KeyCode::Enter => self.set_open(None),
                KeyCode::Right | KeyCode::Char('l') => self.set_open(Some(true)),
                KeyCode::Left | KeyCode::Char('h') => self.set_open(Some(false)),
                KeyCode::Char('d') => self.start(),
                KeyCode::Char('x') if self.rx.is_some() => {
                    self.cancel.store(true, Ordering::Relaxed);
                    self.status = Some(("ruším stahování…".to_string(), false));
                }
                _ => {}
            },
        }
        false
    }

    fn row(&self, row: Row) -> Line<'_> {
        let dim = Style::new().fg(Color::DarkGray);
        let green = Style::new().fg(Color::Green);
        match row {
            Row::Group(distro) => {
                let members = self.members(distro);
                let picked = members.iter().filter(|&&i| self.checked[i]).count();
                let (mark, mark_style) = match picked {
                    0 => ("[ ]", dim),
                    n if n == members.len() => ("[x]", green),
                    _ => ("[-]", green),
                };
                let arrow = match self.is_open(distro) {
                    true => '▾',
                    false => '▸',
                };
                let mut summary = format!("obrazů: {}", members.len());
                if picked > 0 {
                    summary += &format!(", vybráno: {picked}");
                }
                // A closed group still shows that something in it is going on.
                let states = || members.iter().map(|&i| &self.states[i]);
                let busy = |s: &&State| matches!(s, State::Resolving | State::Downloading { .. });
                let failed = |s: &&State| matches!(s, State::Failed(_));
                let (state, state_style) = states()
                    .find(busy)
                    .or_else(|| states().find(failed))
                    .map_or((String::new(), dim), state_text);
                Line::from(vec![
                    Span::raw(format!(" {arrow} ")),
                    Span::styled(format!("{mark} "), mark_style),
                    Span::raw(format!("{distro:17}")),
                    Span::styled(format!("{summary:51}"), dim),
                    Span::styled(state, state_style),
                ])
            }
            Row::Image(i) => {
                let image = &self.images[i];
                let (mark, mark_style) = match self.checked[i] {
                    true => ("[x]", green),
                    false => ("[ ]", dim),
                };
                // Inside a group the header already names the distribution.
                let distro = match self.is_group(image.distro) {
                    true => "",
                    false => image.distro,
                };
                let (state, state_style) = state_text(&self.states[i]);
                Line::from(vec![
                    Span::styled(format!("   {mark} "), mark_style),
                    Span::raw(format!("{distro:17}")),
                    Span::raw(format!("{:26}", image.edition)),
                    Span::styled(format!("{:25}", image.id), dim),
                    Span::styled(state, state_style),
                ])
            }
        }
    }

    fn draw(&mut self, f: &mut Frame) {
        let dim = Style::new().fg(Color::DarkGray);
        let [head, main, gauge, status, help] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(u16::from(self.current.is_some())),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(f.area());

        let picked = self.checked.iter().filter(|c| **c).count();
        let mut top = vec![
            Span::styled(" Složka: ", dim),
            Span::raw(self.cfg.dir.display().to_string()),
            Span::styled(format!("   vybráno: {picked}"), dim),
        ];
        if !self.filter.is_empty() && !matches!(self.mode, Mode::Filter) {
            top.push(Span::styled(format!("   filtr: {}", self.filter), dim));
        }
        f.render_widget(Line::from(top), head);

        let shown = self.matching().len();
        let title = format!(" {NAME}: {shown} z {} ", self.images.len());
        let block = Block::bordered().title(title);
        if self.rows.is_empty() {
            let hint = Paragraph::new(Line::styled(" Filtru nic neodpovídá.", dim));
            f.render_widget(hint.block(block), main);
        } else {
            let mut state = self.list.clone();
            let list = List::new(self.rows.iter().map(|&row| self.row(row)))
                .block(block)
                .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
            f.render_stateful_widget(list, main, &mut state);
            self.list = state;
        }

        if let Some(cur) = &self.current {
            let progress = self.states.iter().find_map(|s| match s {
                State::Downloading { done, total } => Some((*done, *total)),
                _ => None,
            });
            let (done, total) = progress.unwrap_or((0, None));
            let mut label = format!("{}  {}", cur.name, net::human(done));
            if let Some(total) = total {
                label += &format!(" z {}", net::human(total));
            }
            if cur.speed > 0.0 {
                label += &format!("  {}/s", net::human(cur.speed as u64));
                if let Some(total) = total {
                    let left = total.saturating_sub(done) as f64 / cur.speed;
                    label += &format!("  zbývá {}", clock(left));
                }
            }
            let ratio = match total {
                Some(total) if total > 0 => (done as f64 / total as f64).min(1.0),
                _ => 0.0,
            };
            let bar = Gauge::default()
                .gauge_style(Style::new().fg(Color::Cyan).bg(Color::DarkGray))
                .ratio(ratio)
                .label(label);
            f.render_widget(bar, gauge);
        }

        let failure = match self.rows.get(self.sel()) {
            Some(Row::Image(i)) => match &self.states[*i] {
                State::Failed(e) => Some(e),
                _ => None,
            },
            _ => None,
        };
        let line = match (&self.mode, &self.status) {
            (Mode::Filter, _) => Line::from(vec![
                Span::styled(" Filtr: ", Style::new().fg(Color::Cyan)),
                Span::raw(format!("{}█", self.filter)),
            ]),
            (Mode::Dir(text), _) => Line::from(vec![
                Span::styled(" Složka pro obrazy: ", Style::new().fg(Color::Cyan)),
                Span::raw(format!("{text}█")),
            ]),
            (_, Some((msg, true))) => Line::styled(format!(" {msg}"), Style::new().fg(Color::Red)),
            (_, Some((msg, false))) => {
                Line::styled(format!(" {msg}"), Style::new().fg(Color::Green))
            }
            // Why the selected image failed, as the list has no room for it.
            (_, None) => match failure {
                Some(e) => Line::styled(format!(" {e}"), Style::new().fg(Color::Red)),
                None => Line::default(),
            },
        };
        f.render_widget(line, status);

        let keys = match self.mode {
            Mode::Main if self.rx.is_some() => {
                " Enter rozbalit  mezerník vybrat  / filtr  x zrušit stahování  q konec"
            }
            Mode::Main => {
                " Enter rozbalit  mezerník vybrat  a vše  / filtr  o složka  d stáhnout  q konec"
            }
            Mode::Filter => " Enter potvrdit   Esc zrušit filtr",
            Mode::Dir(_) => " Enter potvrdit   Esc zrušit",
        };
        f.render_widget(Line::styled(keys, dim), help);
    }

    fn event_loop(&mut self, term: &mut DefaultTerminal) -> io::Result<()> {
        loop {
            self.drain();
            term.draw(|f| self.draw(f))?;
            // Without a download nothing changes on its own, so just wait for a key.
            if self.rx.is_some() && !event::poll(TICK)? {
                continue;
            }
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind == KeyEventKind::Press && self.on_key(key.code, key.modifiers) {
                return Ok(());
            }
        }
    }
}

pub fn run(path: PathBuf, cfg: Config) -> Result<(), String> {
    let images = catalog::all();
    let mut app = App {
        path,
        cfg,
        checked: vec![false; images.len()],
        states: images.iter().map(|_| State::Idle).collect(),
        rows: Vec::new(),
        open: Vec::new(),
        images,
        filter: String::new(),
        list: ListState::default().with_selected(Some(0)),
        mode: Mode::Main,
        status: None,
        rx: None,
        cancel: Arc::new(AtomicBool::new(false)),
        current: None,
        quitting: false,
    };
    app.rebuild();
    let mut term = ratatui::init();
    let result = app.event_loop(&mut term);
    ratatui::restore();
    result.map_err(|e| format!("terminál: {e}"))
}
