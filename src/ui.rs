use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

use crate::catalog::Catalog;
use crate::cli::Report;
use crate::detect::Family;
use crate::exec::Runner;
use crate::privilege;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Padding, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};

type Detector = Arc<dyn Fn() -> Result<Report, String> + Send + Sync>;

pub const MIN_COLS: u16 = 80;
pub const MIN_ROWS: u16 = 24;
const LOG_LINES: usize = 4;

/// One small palette, so every panel and mark draws from the same theme.
mod theme {
    use ratatui::style::{Color, Modifier, Style};

    pub const ACCENT: Color = Color::Cyan;

    pub fn header() -> Style {
        Style::new()
            .bg(ACCENT)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD)
    }

    pub fn accent() -> Style {
        Style::new().fg(ACCENT)
    }

    pub fn title() -> Style {
        Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)
    }

    pub fn ok() -> Style {
        Style::new().fg(Color::Green).add_modifier(Modifier::BOLD)
    }

    pub fn bad() -> Style {
        Style::new().fg(Color::Red).add_modifier(Modifier::BOLD)
    }

    pub fn muted() -> Style {
        Style::new().fg(Color::DarkGray)
    }

    pub fn selection() -> Style {
        Style::new()
            .bg(ACCENT)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD)
    }
}

/// A bordered panel with a bold accent title and one column of breathing room.
fn panel(title: &str) -> Block<'static> {
    Block::bordered()
        .border_style(theme::accent())
        .title(Line::from(format!(" {title} ")).style(theme::title()))
        .padding(Padding::new(1, 1, 0, 0))
}

pub fn too_small(cols: u16, rows: u16) -> bool {
    cols < MIN_COLS || rows < MIN_ROWS
}

/// Run the terminal UI. Detection is delegated to worker thread.
pub fn run<F>(detect: F) -> i32
where
    F: Fn() -> Result<Report, String> + Send + Sync + 'static,
{
    match too_small_size() {
        Ok(Some((cols, rows))) => {
            eprintln!(
                "resolve-linux: terminal too small ({cols}x{rows}); at least {MIN_COLS}x{MIN_ROWS} is required."
            );
            return 0;
        }
        Ok(None) => {}
        Err(error) => {
            eprintln!("resolve-linux: cannot read terminal size: {error}");
            return 1;
        }
    }

    let detector: Detector = Arc::new(detect);
    let (sender, receiver) = mpsc::channel();
    spawn_detection(detector.clone(), sender);

    let mut terminal = match ratatui::try_init() {
        Ok(terminal) => terminal,
        Err(error) => {
            eprintln!("resolve-linux: cannot start the terminal UI: {error}");
            return 1;
        }
    };

    let mut app = App::new(receiver, detector);
    let result = event_loop(&mut terminal, &mut app);
    ratatui::restore();

    match result {
        Ok(Some((cols, rows))) => {
            eprintln!(
                "resolve-linux: terminal too small ({cols}x{rows}); at least {MIN_COLS}x{MIN_ROWS} is required."
            );
            0
        }
        Ok(None) => 0,
        Err(error) => {
            eprintln!("resolve-linux: terminal UI failed: {error}");
            1
        }
    }
}

fn spawn_detection(detector: Detector, sender: mpsc::Sender<Result<Report, String>>) {
    std::thread::spawn(move || {
        let _ = sender.send(detector());
    });
}

fn too_small_size() -> std::io::Result<Option<(u16, u16)>> {
    let (cols, rows) = terminal::size()?;
    Ok(too_small(cols, rows).then_some((cols, rows)))
}

fn row(label: &str, value: impl std::fmt::Display) -> Line<'static> {
    Line::from(format!("{label:<19}{value}"))
}

/// A row whose leading mark carries colour, with words that stand on their own.
fn mark_row(label: &str, mark: char, style: Style, value: impl Into<String>) -> Line<'static> {
    Line::from(vec![
        Span::raw(format!("{label:<19}")),
        Span::styled(format!("{mark} "), style),
        Span::raw(value.into()),
    ])
}

fn log_line(entry: &str) -> Line<'static> {
    match entry.split_once(' ') {
        Some((tag, message)) if tag.starts_with('[') && tag.ends_with(']') => Line::from(vec![
            Span::styled(
                format!("{tag} "),
                theme::accent().add_modifier(Modifier::BOLD),
            ),
            Span::raw(message.to_owned()),
        ]),
        _ => Line::from(entry.to_owned()),
    }
}

fn event_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
) -> std::io::Result<Option<(u16, u16)>> {
    let mut dirty = true;
    loop {
        dirty |= app.poll_detection();
        if dirty {
            terminal.draw(|frame| app.draw(frame))?;
            dirty = false;
        }
        if !app.running {
            break;
        }

        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    app.handle_key(key);
                    if let Some(action) = app.take_action() {
                        match action {
                            Action::InstallDependencies => install_dependencies(terminal, app)?,
                        }
                    }
                    dirty = true;
                }
                Event::Resize(cols, rows) => {
                    if too_small(cols, rows) {
                        return Ok(Some((cols, rows)));
                    }
                    dirty = true;
                }
                _ => {}
            }
        }
    }
    Ok(None)
}

/// Run the elevated install with terminal attached, then restore the
/// alternate screen and reload the Dependency Set
fn install_dependencies(terminal: &mut DefaultTerminal, app: &mut App) -> std::io::Result<()> {
    let Some(report) = app.report.as_ref() else {
        app.log_action("install", "detection has not finished");
        return Ok(());
    };

    let Some(family) = Family::from_name(&report.distro.family) else {
        app.log_action(
            "install",
            format!("refused: unsupported distribution `{}`", report.distro.id),
        );
        return Ok(());
    };

    let missing: Vec<String> = report
        .dependencies
        .iter()
        .filter(|dependency| !dependency.present)
        .map(|dependency| dependency.package.clone())
        .collect();

    if missing.is_empty() {
        app.log_action("install", "all dependencies are present");
        return Ok(());
    }

    let command = privilege::escalate(Catalog::for_family(family).install(&missing));

    if let Err(error) = privilege::ensure_terminal(&command) {
        app.log_action("install", error.to_string());
        return Ok(());
    }

    app.log_action("install", command.display());
    ratatui::restore();
    let status = Runner::new(false).run_interactive(&command);
    *terminal = ratatui::try_init()?;

    app.record_install_result(&command.display(), status);
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Screen {
    Status,
    Dependencies,
    InstallResolve,
    PostInstallPatch,
    Gpu,
}

impl Screen {
    const ALL: [Screen; 5] = [
        Screen::Status,
        Screen::Dependencies,
        Screen::InstallResolve,
        Screen::PostInstallPatch,
        Screen::Gpu,
    ];

    fn at(index: usize) -> Screen {
        Self::ALL[index.min(Self::ALL.len() - 1)]
    }

    fn title(self) -> &'static str {
        match self {
            Screen::Status => "Status",
            Screen::Dependencies => "Dependencies",
            Screen::InstallResolve => "Install Resolve",
            Screen::PostInstallPatch => "Post-install Patch",
            Screen::Gpu => "GPU",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum View {
    Menu,
    Screen(Screen),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    InstallDependencies,
}

struct App {
    view: View,
    menu_state: ListState,
    report: Option<Report>,
    detection_error: Option<String>,
    detection_done: bool,
    receiver: Receiver<Result<Report, String>>,
    detector: Detector,
    action: Option<Action>,
    log: Vec<String>,
    log_offset: usize,
    running: bool,
}

impl App {
    fn new(receiver: Receiver<Result<Report, String>>, detector: Detector) -> Self {
        let mut menu_state = ListState::default();
        menu_state.select(Some(0));
        let mut app = Self {
            view: View::Menu,
            menu_state,
            report: None,
            detection_error: None,
            detection_done: false,
            receiver,
            detector,
            action: None,
            log: Vec::new(),
            log_offset: 0,
            running: true,
        };
        app.log_action("status", "detection started");
        app
    }

    fn take_action(&mut self) -> Option<Action> {
        self.action.take()
    }

    fn refresh(&mut self) {
        let (sender, receiver) = mpsc::channel();
        self.receiver = receiver;
        self.report = None;
        self.detection_done = false;
        self.detection_error = None;
        spawn_detection(self.detector.clone(), sender);
        self.log_action("status", "refreshing");
    }

    fn record_install_result(
        &mut self,
        command: &str,
        status: std::io::Result<std::process::ExitStatus>,
    ) {
        match status {
            Ok(status) => self.log_command("install", command, status.code()),
            Err(error) => self.log_action("install", format!("cannot run sudo: {error}")),
        }
        self.refresh();
    }

    fn request_install(&mut self) {
        let Some(report) = &self.report else {
            self.log_action("install", "detection has not finished");
            return;
        };
        if !report.distro.supported {
            self.log_action(
                "install",
                format!("refused: unsupported distribution `{}`", report.distro.id),
            );
            return;
        }
        if report
            .dependencies
            .iter()
            .all(|dependency| dependency.present)
        {
            self.log_action("install", "all dependencies are present");
            return;
        }
        self.action = Some(Action::InstallDependencies);
    }

    fn poll_detection(&mut self) -> bool {
        if self.detection_done {
            return false;
        }
        match self.receiver.try_recv() {
            Ok(Ok(report)) => {
                let probes = report.probe_log.clone();
                self.report = Some(report);
                for probe in probes {
                    self.log_command("probe", &probe.command, probe.exit_code);
                }
                self.detection_done = true;
                self.log_action("status", "detection complete");
                true
            }
            Ok(Err(error)) => {
                self.detection_error = Some(error.clone());
                self.detection_done = true;
                self.log_action("status", format!("detection failed: {error}"));
                true
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                self.detection_error = Some("detection worker stopped unexpectedly".to_owned());
                self.detection_done = true;
                self.log_action("status", "detection failed: worker stopped unexpectedly");
                true
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.running = false;
            return;
        }

        match key.code {
            KeyCode::PageUp => {
                let max = self.log.len().saturating_sub(LOG_LINES);
                self.log_offset = (self.log_offset + LOG_LINES).min(max);
                return;
            }
            KeyCode::PageDown => {
                self.log_offset = self.log_offset.saturating_sub(LOG_LINES);
                return;
            }
            _ => {}
        }

        match self.view {
            View::Menu => match key.code {
                KeyCode::Char('q') => self.running = false,
                KeyCode::Up => self.select_previous(),
                KeyCode::Down => self.select_next(),
                KeyCode::Enter => {
                    let screen = Screen::at(self.menu_state.selected().unwrap_or(0));
                    self.view = View::Screen(screen);
                    self.log_action("nav", screen.title());
                }
                _ => {}
            },
            View::Screen(screen) => match key.code {
                KeyCode::Esc => {
                    self.view = View::Menu;
                    self.log_action("nav", "main menu");
                }
                KeyCode::Char('i') if screen == Screen::Dependencies => {
                    self.request_install();
                }
                _ => {}
            },
        }
    }

    fn select_previous(&mut self) {
        let selected = self.menu_state.selected().unwrap_or(0);
        let previous = if selected == 0 {
            Screen::ALL.len() - 1
        } else {
            selected - 1
        };
        self.menu_state.select(Some(previous));
    }

    fn select_next(&mut self) {
        let selected = self.menu_state.selected().unwrap_or(0);
        self.menu_state
            .select(Some((selected + 1) % Screen::ALL.len()));
    }

    fn log_action(&mut self, action: &str, message: impl AsRef<str>) {
        self.log.push(format!("[{action}] {}", message.as_ref()));
        self.log_offset = 0;
    }

    fn log_command(&mut self, action: &str, command: &str, exit_code: Option<i32>) {
        let exit = exit_code
            .map(|code| format!("exit {code}"))
            .unwrap_or_else(|| "exit unknown".to_owned());
        self.log.push(format!("[{action}] {command} → {exit}"));
        self.log_offset = 0;
    }

    fn draw(&mut self, frame: &mut Frame) {
        let chunks = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(10),
            Constraint::Length(6),
            Constraint::Length(1),
        ])
        .split(frame.area());

        frame.render_widget(
            Paragraph::new(" resolve-linux  ·  DaVinci Resolve install helper")
                .style(theme::header()),
            chunks[0],
        );

        match self.view {
            View::Menu => self.draw_menu(frame, chunks[1]),
            View::Screen(screen) => self.draw_screen(frame, chunks[1], screen),
        }

        self.draw_log(frame, chunks[2]);

        let footer = match self.view {
            View::Menu => "Up/Down select · Enter open · PgUp/PgDn log · q quit",
            View::Screen(Screen::Dependencies) => {
                "i install missing · Esc back · PgUp/PgDn log · Ctrl-C quit"
            }
            View::Screen(_) => "Esc back · PgUp/PgDn log · Ctrl-C quit",
        };
        frame.render_widget(Paragraph::new(footer).style(theme::muted()), chunks[3]);
    }

    fn draw_menu(&mut self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = Screen::ALL
            .iter()
            .map(|screen| ListItem::new(screen.title()))
            .collect();
        let list = List::new(items)
            .block(panel("Main menu"))
            .highlight_style(theme::selection())
            .highlight_symbol("› ");
        frame.render_stateful_widget(list, area, &mut self.menu_state);
    }

    fn draw_screen(&mut self, frame: &mut Frame, area: Rect, screen: Screen) {
        let lines = match screen {
            Screen::Status => self.status_lines(),
            Screen::Dependencies => self.dependencies_lines(),
            _ => vec![
                Line::from("This screen arrives in a later release."),
                Line::from("Press Esc to return to the menu."),
            ],
        };
        let paragraph = Paragraph::new(lines)
            .block(panel(screen.title()))
            .wrap(Wrap { trim: false });
        frame.render_widget(paragraph, area);
    }

    fn detection_placeholder(&self) -> Vec<Line<'static>> {
        if let Some(error) = &self.detection_error {
            return vec![mark_row(
                "Status",
                '✗',
                theme::bad(),
                format!("Detection failed: {error}"),
            )];
        }
        vec![Line::from(vec![
            Span::styled("◐ ", theme::accent()),
            Span::styled("Detecting distribution…", theme::accent()),
        ])]
    }

    fn status_lines(&self) -> Vec<Line<'static>> {
        let Some(report) = &self.report else {
            return self.detection_placeholder();
        };

        let mut lines = Vec::new();
        lines.push(row(
            "Distribution",
            format!("{} ({})", report.distro.name, report.distro.id),
        ));
        lines.push(if report.distro.supported {
            mark_row("Support", '✓', theme::ok(), "supported")
        } else {
            mark_row("Support", '✗', theme::bad(), "unsupported")
        });
        lines.push(row("Family", &report.distro.family));
        match &report.package_manager {
            Some(package_manager) => lines.push(row("Package manager", package_manager)),
            None => lines.push(mark_row(
                "Package manager",
                '✗',
                theme::bad(),
                "none — unsupported distribution",
            )),
        }

        if !report.distro.supported && !report.distro.manual_packages.is_empty() {
            lines.push(row(
                "Manual packages",
                report.distro.manual_packages.join(", "),
            ));
        }

        if report.resolve.installed {
            let version = report.resolve.version.as_deref().unwrap_or("unknown");
            let tested = if report.resolve.tested {
                "tested"
            } else {
                "untested"
            };
            lines.push(mark_row(
                "Resolve",
                '✓',
                theme::ok(),
                format!("installed (version {version}, {tested})"),
            ));
        } else {
            lines.push(mark_row("Resolve", '✗', theme::bad(), "not installed"));
        }

        if report.dependencies.is_empty() {
            lines.push(mark_row(
                "Dependencies",
                '—',
                theme::muted(),
                "not checked in this build",
            ));
        } else {
            lines.push(row(
                "Dependencies",
                format!("{} checked", report.dependencies.len()),
            ));
        }

        if report.gpu.vendors.is_empty() {
            lines.push(mark_row(
                "GPU",
                '—',
                theme::muted(),
                "not checked in this build",
            ));
        } else {
            lines.push(row("GPU", report.gpu.vendors.join(", ")));
        }

        lines.push(if report.patch.applicable {
            mark_row("Post-install patch", '✓', theme::ok(), "applicable")
        } else {
            mark_row(
                "Post-install patch",
                '—',
                theme::muted(),
                "not checked in this build",
            )
        });

        lines
    }

    fn dependencies_lines(&self) -> Vec<Line<'static>> {
        let Some(report) = &self.report else {
            return self.detection_placeholder();
        };

        if !report.distro.supported {
            let mut lines = vec![mark_row(
                "Support",
                '✗',
                theme::bad(),
                "unsupported distribution",
            )];
            lines.push(Line::from(""));
            lines.push(Line::from("Install these packages manually:"));
            for package in &report.distro.manual_packages {
                lines.push(Line::from(format!("  • {package}")));
            }
            lines.push(Line::from(""));
            lines.push(Line::from("No package-manager commands will be run."));
            return lines;
        }

        let mut lines = vec![Line::from(vec![
            Span::styled(
                format!("{:<14}{:<24}", "Dependency", "Package"),
                theme::title(),
            ),
            Span::styled("Status".to_owned(), theme::title()),
        ])];
        for dependency in &report.dependencies {
            let (mark, style, word) = if dependency.present {
                ('✓', theme::ok(), "present")
            } else {
                ('✗', theme::bad(), "missing")
            };
            lines.push(Line::from(vec![
                Span::raw(format!("{:<14}", dependency.name)),
                Span::raw(format!("{:<24}", dependency.package)),
                Span::styled(format!("{mark} "), style),
                Span::raw(word),
            ]));
        }
        if report
            .dependencies
            .iter()
            .any(|dependency| !dependency.present)
        {
            lines.push(Line::from(""));
            lines.push(Line::from(
                "Press i to install the missing packages with sudo.",
            ));
        }
        lines
    }

    fn draw_log(&self, frame: &mut Frame, area: Rect) {
        let lines: Vec<Line> = if self.log.is_empty() {
            vec![Line::from("No commands run yet.").style(theme::muted())]
        } else {
            let end = self.log.len().saturating_sub(self.log_offset);
            let start = end.saturating_sub(LOG_LINES);
            self.log[start..end]
                .iter()
                .map(|entry| log_line(entry))
                .collect()
        };
        frame.render_widget(
            Paragraph::new(lines)
                .block(panel("Log"))
                .wrap(Wrap { trim: false }),
            area,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{
        DependencySection, DistroSection, GpuSection, PatchSection, ProbeRecord, ResolveSection,
    };
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;

    fn report_supported() -> Report {
        Report {
            schema_version: 1,
            distro: DistroSection {
                id: "fedora".to_owned(),
                name: "Fedora Linux".to_owned(),
                family: "fedora".to_owned(),
                supported: true,
                manual_packages: Vec::new(),
            },
            package_manager: Some("dnf".to_owned()),
            resolve: ResolveSection {
                installed: false,
                version: None,
                tested: false,
            },
            dependencies: Vec::new(),
            gpu: GpuSection {
                vendors: Vec::new(),
            },
            patch: PatchSection { applicable: false },
            probe_log: Vec::new(),
        }
    }

    fn report_with_dependencies() -> Report {
        Report {
            dependencies: vec![
                DependencySection {
                    name: "glib".to_owned(),
                    package: "glib2".to_owned(),
                    present: true,
                },
                DependencySection {
                    name: "vulkan".to_owned(),
                    package: "vulkan-loader".to_owned(),
                    present: false,
                },
            ],
            ..report_supported()
        }
    }

    fn report_unsupported() -> Report {
        Report {
            distro: DistroSection {
                id: "opensuse-tumbleweed".to_owned(),
                name: "openSUSE Tumbleweed".to_owned(),
                family: "unsupported".to_owned(),
                supported: false,
                manual_packages: vec!["glib".to_owned(), "opencl".to_owned()],
            },
            package_manager: None,
            ..report_supported()
        }
    }

    fn detector_for(report: Option<Report>) -> Detector {
        Arc::new(move || {
            report
                .clone()
                .ok_or_else(|| "no detection result configured".to_owned())
        })
    }

    fn app_with(report: Option<Report>) -> App {
        let (_sender, receiver) = mpsc::channel();
        let mut app = App::new(receiver, detector_for(None));
        app.report = report;
        app.detection_done = true;
        app
    }

    fn draw_buffer(app: &mut App, width: u16, height: u16) -> ratatui::buffer::Buffer {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal.draw(|frame| app.draw(frame)).expect("draw frame");
        terminal.backend().buffer().clone()
    }

    fn render(app: &mut App, width: u16, height: u16) -> String {
        let buffer = draw_buffer(app, width, height);
        let mut lines = Vec::new();
        for y in 0..buffer.area.height {
            let mut line = String::new();
            for x in 0..buffer.area.width {
                line.push_str(buffer[(x, y)].symbol());
            }
            lines.push(line.trim_end().to_owned());
        }
        lines.join("\n")
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn the_menu_has_exactly_five_entries_in_order() {
        assert_eq!(Screen::ALL.len(), 5);
        assert_eq!(
            Screen::ALL.map(Screen::title),
            [
                "Status",
                "Dependencies",
                "Install Resolve",
                "Post-install Patch",
                "GPU"
            ]
        );

        let mut app = app_with(None);
        let text = render(&mut app, 80, 24);
        for screen in Screen::ALL {
            assert!(
                text.contains(screen.title()),
                "missing {}: {text}",
                screen.title()
            );
        }
    }

    #[test]
    fn status_shows_family_and_package_manager() {
        let mut app = app_with(Some(report_supported()));
        app.view = View::Screen(Screen::Status);
        let text = render(&mut app, 80, 24);

        assert!(text.contains("fedora"), "{text}");
        assert!(text.contains("dnf"), "{text}");
        assert!(text.contains("✓ supported"), "{text}");
    }

    #[test]
    fn status_states_unsupported_distributions_in_words() {
        let mut app = app_with(Some(report_unsupported()));
        app.view = View::Screen(Screen::Status);
        let text = render(&mut app, 80, 24);

        assert!(text.contains("✗ unsupported"), "{text}");
        assert!(text.contains("Package manager"), "{text}");
        assert!(text.contains("✗ none — unsupported distribution"), "{text}");
        assert!(text.contains("glib, opencl"), "{text}");
    }

    #[test]
    fn dependencies_screen_shows_one_row_per_dependency_with_symbol_and_word() {
        let mut app = app_with(Some(report_with_dependencies()));
        app.view = View::Screen(Screen::Dependencies);
        let text = render(&mut app, 80, 24);

        assert!(text.contains("Dependency"), "{text}");
        assert!(text.contains("glib"), "{text}");
        assert!(text.contains("glib2"), "{text}");
        assert!(text.contains("✓ present"), "{text}");
        assert!(text.contains("vulkan"), "{text}");
        assert!(text.contains("vulkan-loader"), "{text}");
        assert!(text.contains("✗ missing"), "{text}");
    }

    #[test]
    fn dependencies_screen_shows_the_manual_list_for_unsupported_distributions() {
        let mut app = app_with(Some(report_unsupported()));
        app.view = View::Screen(Screen::Dependencies);
        let text = render(&mut app, 80, 24);

        assert!(text.contains("unsupported"), "{text}");
        assert!(text.contains("Install these packages manually"), "{text}");
        assert!(text.contains("glib"), "{text}");
        assert!(text.contains("opencl"), "{text}");
        assert!(
            !text.contains("Package"),
            "unsupported distros must not show a table: {text}"
        );
    }

    #[test]
    fn dependencies_screen_renders_the_detecting_frame() {
        let mut app = app_with(None);
        app.view = View::Screen(Screen::Dependencies);
        let text = render(&mut app, 80, 24);
        assert!(text.contains("Detecting distribution"), "{text}");
    }

    #[test]
    fn the_dependencies_screen_offers_install_only_when_something_is_missing() {
        let mut app = app_with(Some(report_with_dependencies()));
        app.view = View::Screen(Screen::Dependencies);
        let text = render(&mut app, 80, 24);
        assert!(text.contains("Press i to install"), "{text}");

        app.handle_key(key(KeyCode::Char('i')));
        assert_eq!(app.take_action(), Some(Action::InstallDependencies));

        let mut app = app_with(Some(report_with_dependencies()));
        app.view = View::Screen(Screen::Dependencies);
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(
            app.take_action(),
            None,
            "Enter must not start a privileged install"
        );

        let mut complete = report_with_dependencies();
        for dependency in &mut complete.dependencies {
            dependency.present = true;
        }
        let mut app = app_with(Some(complete));
        app.view = View::Screen(Screen::Dependencies);
        app.handle_key(key(KeyCode::Char('i')));
        assert_eq!(app.take_action(), None);
        assert!(
            app.log
                .iter()
                .any(|entry| entry.contains("all dependencies are present")),
            "{:?}",
            app.log
        );
    }

    #[test]
    fn unsupported_distributions_never_queue_an_install() {
        let mut app = app_with(Some(report_unsupported()));
        app.view = View::Screen(Screen::Dependencies);
        app.handle_key(key(KeyCode::Char('i')));

        assert_eq!(app.take_action(), None);
        assert!(
            app.log
                .iter()
                .any(|entry| entry.contains("refused: unsupported")),
            "{:?}",
            app.log
        );
    }

    #[test]
    fn a_failed_install_is_logged_and_still_refreshes_the_dependency_set() {
        let mut app = app_with(None);
        app.detection_done = true;
        app.record_install_result(
            "LC_ALL=C sudo dnf install -y glib2",
            Ok(ExitStatus::from_raw(1 << 8)),
        );

        assert!(
            app.log.iter().any(|entry| entry.contains("exit 1")),
            "{:?}",
            app.log
        );
        assert!(
            !app.detection_done,
            "the table must reload after a failed install"
        );
    }

    #[test]
    fn refresh_reloads_the_report_from_the_detector() {
        let detector: Detector = Arc::new(|| Ok(report_with_dependencies()));
        let (_sender, receiver) = mpsc::channel();
        let mut app = App::new(receiver, detector);
        app.report = Some(report_supported());
        app.view = View::Screen(Screen::Dependencies);

        app.refresh();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !app.poll_detection() {
            assert!(
                std::time::Instant::now() < deadline,
                "detection did not refresh in time"
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        let text = render(&mut app, 80, 24);
        assert!(text.contains("vulkan-loader"), "{text}");
        assert!(text.contains("✓ present"), "{text}");
    }

    #[test]
    fn a_failed_refresh_drops_the_stale_dependency_table() {
        let detector: Detector = Arc::new(|| Err("probe failed".to_owned()));
        let (_sender, receiver) = mpsc::channel();
        let mut app = App::new(receiver, detector);
        app.report = Some(report_with_dependencies());
        app.detection_done = true;
        app.view = View::Screen(Screen::Dependencies);

        app.refresh();
        assert!(
            app.report.is_none(),
            "a refresh must not keep the pre-install table on screen"
        );

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !app.poll_detection() {
            assert!(
                std::time::Instant::now() < deadline,
                "detection did not fail in time"
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        let text = render(&mut app, 80, 24);
        assert!(text.contains("Detection failed"), "{text}");
        assert!(
            !text.contains("vulkan-loader"),
            "the stale dependency row survived a failed refresh: {text}"
        );
    }

    #[test]
    fn status_renders_the_detecting_frame_before_the_worker_finishes() {
        let mut app = app_with(None);
        app.view = View::Screen(Screen::Status);
        let text = render(&mut app, 80, 24);
        assert!(text.contains("Detecting distribution"), "{text}");
    }

    #[test]
    fn a_late_detection_result_requests_a_repaint_with_the_report() {
        let (sender, receiver) = mpsc::channel();
        let mut app = App::new(receiver, detector_for(None));
        app.view = View::Screen(Screen::Status);

        let before = render(&mut app, 80, 24);
        assert!(before.contains("Detecting distribution"), "{before}");

        sender
            .send(Ok(report_supported()))
            .expect("receiver is alive");
        assert!(app.poll_detection(), "a result must invalidate the frame");

        let after = render(&mut app, 80, 24);
        assert!(after.contains("Fedora Linux"), "{after}");
        assert!(after.contains("dnf"), "{after}");
    }

    #[test]
    fn detection_writes_each_probe_to_the_log_pane() {
        let (sender, receiver) = mpsc::channel();
        let mut app = App::new(receiver, detector_for(None));
        let mut report = report_supported();
        report.probe_log = vec![ProbeRecord {
            command: "LC_ALL=C rpm -q glib2".to_owned(),
            exit_code: Some(0),
        }];

        sender.send(Ok(report)).expect("receiver is alive");
        assert!(app.poll_detection());

        let text = render(&mut app, 80, 24);
        assert!(text.contains("[probe]"), "{text}");
        assert!(text.contains("rpm -q glib2"), "{text}");
        assert!(text.contains("exit 0"), "{text}");
    }

    #[test]
    fn a_late_detection_error_requests_a_repaint_with_the_error() {
        let (sender, receiver) = mpsc::channel();
        let mut app = App::new(receiver, detector_for(None));
        app.view = View::Screen(Screen::Status);

        sender
            .send(Err("cannot read /nonexistent/os-release".to_owned()))
            .expect("receiver is alive");
        assert!(app.poll_detection(), "an error must invalidate the frame");

        let text = render(&mut app, 80, 24);
        assert!(text.contains("Detection failed"), "{text}");
        assert!(text.contains("/nonexistent/os-release"), "{text}");
        assert!(app.detection_error.is_some());
    }

    #[test]
    fn the_log_pane_names_the_action_command_and_exit_code() {
        let mut app = app_with(None);
        app.log_command("install dependencies", "dnf install -y glib", Some(0));
        let text = render(&mut app, 80, 24);

        assert!(text.contains("install dependencies"), "{text}");
        assert!(text.contains("dnf install -y glib"), "{text}");
        assert!(text.contains("exit 0"), "{text}");
    }

    #[test]
    fn older_log_entries_are_retained_and_reachable_with_page_up() {
        let mut app = app_with(None);
        for index in 0..6 {
            app.log_action("command", format!("command {index}"));
        }

        let latest = render(&mut app, 80, 24);
        assert!(latest.contains("command 5"), "{latest}");
        assert!(!latest.contains("command 0"), "{latest}");

        app.handle_key(key(KeyCode::PageUp));
        let older = render(&mut app, 80, 24);
        assert!(older.contains("command 0"), "{older}");
        assert!(older.contains("command 1"), "{older}");
    }

    #[test]
    fn escape_returns_to_the_menu_from_any_screen() {
        let mut app = app_with(None);
        for screen in Screen::ALL {
            app.view = View::Screen(screen);
            app.handle_key(key(KeyCode::Esc));
            assert_eq!(
                app.view,
                View::Menu,
                "{screen:?} did not return to the menu"
            );
        }
    }

    #[test]
    fn q_quits_from_the_menu_and_only_from_the_menu() {
        let mut app = app_with(None);
        app.view = View::Screen(Screen::Status);
        app.handle_key(key(KeyCode::Char('q')));
        assert!(app.running, "q on a screen must not quit");
        assert_eq!(app.view, View::Screen(Screen::Status));

        app.view = View::Menu;
        app.handle_key(key(KeyCode::Char('q')));
        assert!(!app.running);
    }

    #[test]
    fn menu_selection_wraps_in_both_directions() {
        let mut app = app_with(None);
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.menu_state.selected(), Some(4));
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.menu_state.selected(), Some(0));
    }

    #[test]
    fn the_size_guard_requires_eighty_by_twenty_four() {
        assert!(!too_small(80, 24));
        assert!(!too_small(120, 40));
        assert!(too_small(79, 24));
        assert!(too_small(80, 23));
        assert!(too_small(60, 20));
    }

    #[test]
    fn the_header_is_an_accent_bar_and_marks_keep_their_words() {
        let mut app = app_with(Some(report_supported()));
        app.view = View::Screen(Screen::Status);
        let buffer = draw_buffer(&mut app, 80, 24);

        let header_end = &buffer[(79, 0)];
        assert_eq!(header_end.style().bg, Some(theme::ACCENT));
        assert_eq!(header_end.style().fg, Some(Color::Black));

        let mark = buffer
            .content()
            .iter()
            .find(|cell| cell.symbol() == "✓")
            .expect("the supported row shows a ✓ mark");
        assert_eq!(mark.style().fg, Some(Color::Green));

        let text = render(&mut app, 80, 24);
        assert!(text.contains("✓ supported"), "{text}");
    }

    #[test]
    fn the_layout_does_not_clip_at_eighty_by_twenty_four() {
        let mut app = app_with(Some(report_unsupported()));
        app.view = View::Screen(Screen::Status);
        let text = render(&mut app, 80, 24);
        let last = text.lines().last().unwrap_or_default();
        assert!(last.contains("Esc back"), "footer was clipped: {text}");
        assert!(
            text.contains("Post-install patch"),
            "status content was clipped: {text}"
        );
    }
}
