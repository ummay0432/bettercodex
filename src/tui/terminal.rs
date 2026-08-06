use anyhow::Context;
use anyhow::Result;
use crossterm::cursor::MoveTo;
use crossterm::cursor::SetCursorStyle;
use crossterm::event::DisableBracketedPaste;
use crossterm::event::EnableBracketedPaste;
use crossterm::event::KeyboardEnhancementFlags;
use crossterm::event::PopKeyboardEnhancementFlags;
use crossterm::event::PushKeyboardEnhancementFlags;
use crossterm::execute;
use crossterm::queue;
use crossterm::style::Attribute;
use crossterm::style::ResetColor;
use crossterm::style::SetAttribute;
use crossterm::style::SetBackgroundColor;
use crossterm::terminal::Clear;
use crossterm::terminal::ClearType;
use crossterm::terminal::ScrollUp;
use crossterm::terminal::disable_raw_mode;
use crossterm::terminal::enable_raw_mode;
use ratatui::Terminal;
use ratatui::TerminalOptions;
use ratatui::Viewport;
use ratatui::backend::Backend;
use ratatui::backend::CrosstermBackend;
use ratatui::backend::IntoCrossterm;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::layout::Size;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use ratatui::widgets::Wrap;
use std::fs::File;
use std::fs::OpenOptions;
use std::io;
use std::io::Stdout;
use std::io::Write;
use std::io::stdout;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::sync::Once;
use std::time::Duration;
use std::time::Instant;

const STARTUP_PROBE_TIMEOUT: Duration = Duration::from_millis(100);
const MAX_HISTORY_ROWS_PER_CELL: usize = 30_000;
const CLEAR_VISIBLE_TERMINAL: &[u8] = b"\x1b[r\x1b[0m\x1b[H\x1b[2J\x1b[H";

/// Codex keeps finalized transcript cells in normal terminal scrollback and owns only the mutable
/// tail plus composer. Ratatui's fixed viewport gives this lean client the same dynamic-height
/// ownership without pulling in Codex's alternate-screen overlays.
pub(super) struct AppTerminal<B: Backend = CrosstermBackend<Stdout>> {
    terminal: Terminal<B>,
    viewport_area: Rect,
    screen_size: Size,
    viewport_top: u16,
}

pub(super) struct TerminalSession {
    terminal: AppTerminal,
    default_foreground: Option<(u8, u8, u8)>,
    default_background: Option<(u8, u8, u8)>,
}

impl TerminalSession {
    pub(super) fn enter() -> Result<Self> {
        install_panic_hook();
        enable_raw_mode().context("failed to enable terminal raw mode")?;
        let mut output = stdout();
        if let Err(error) = execute!(
            output,
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
            ),
            EnableBracketedPaste,
            SetCursorStyle::SteadyBar,
        ) {
            let _ = restore();
            return Err(error).context("failed to initialize terminal input modes");
        }

        let probe = startup_probe(STARTUP_PROBE_TIMEOUT).unwrap_or_default();
        let screen_size = crossterm::terminal::size()
            .map(|(width, height)| Size::new(width, height))
            .context("failed to read terminal size")?;
        output
            .write_all(CLEAR_VISIBLE_TERMINAL)
            .context("failed to clear the terminal before startup")?;
        output
            .flush()
            .context("failed to flush the startup clear")?;

        let viewport_area = Rect::new(0, 0, screen_size.width, 1);
        let terminal = Terminal::with_options(
            CrosstermBackend::new(output),
            TerminalOptions {
                viewport: Viewport::Fixed(viewport_area),
            },
        )
        .context("failed to create inline terminal renderer")?;

        Ok(Self {
            terminal: AppTerminal {
                terminal,
                viewport_area,
                screen_size,
                viewport_top: 0,
            },
            default_foreground: probe.foreground,
            default_background: probe.background,
        })
    }

    pub(super) fn terminal_mut(&mut self) -> &mut AppTerminal {
        &mut self.terminal
    }

    pub(super) fn default_background(&self) -> Option<(u8, u8, u8)> {
        self.default_background
    }

    pub(super) fn default_foreground(&self) -> Option<(u8, u8, u8)> {
        self.default_foreground
    }
}

impl<B> AppTerminal<B>
where
    B: Backend<Error = io::Error> + Write,
{
    pub(super) fn clear_screen(&mut self) -> Result<()> {
        self.refresh_screen_size()?;
        // This is the same reset/home/visible-clear/scrollback-purge sequence Codex uses before
        // replaying source-backed transcript cells after a resize.
        write!(
            self.terminal.backend_mut(),
            "\x1b[r\x1b[0m\x1b[H\x1b[2J\x1b[3J\x1b[H"
        )
        .context("failed to clear terminal history")?;
        Backend::flush(self.terminal.backend_mut())?;
        self.viewport_top = 0;
        self.viewport_area = Rect::new(0, 0, self.screen_size.width, 1);
        self.terminal
            .resize(self.viewport_area)
            .context("failed to reset inline terminal viewport")?;
        Ok(())
    }

    pub(super) fn width(&mut self) -> Result<u16> {
        self.refresh_screen_size()?;
        Ok(self.screen_size.width)
    }

    pub(super) fn height(&mut self) -> Result<u16> {
        self.refresh_screen_size()?;
        Ok(self.screen_size.height)
    }

    pub(super) fn insert_history_lines(
        &mut self,
        lines: Vec<Line<'static>>,
        next_viewport_height: u16,
    ) -> Result<()> {
        if lines.is_empty() {
            return Ok(());
        }
        self.refresh_screen_size()?;
        let width = self.screen_size.width.max(1);
        let next_viewport_height = next_viewport_height.clamp(1, self.screen_size.height.max(1));
        let buffer = render_history_lines(&lines, width);
        let rendered_height = buffer.area.height;

        let history_capacity = self.screen_size.height.saturating_sub(next_viewport_height);
        if history_capacity == 0 {
            for source_y in 0..rendered_height {
                self.draw_buffer_rows(&buffer, source_y, 1, 0)?;
                execute!(self.terminal.backend_mut(), ScrollUp(1))?;
            }
            self.viewport_top = 0;
        } else {
            let mut source_y = 0;
            while source_y < rendered_height {
                let rows = (rendered_height - source_y).min(history_capacity);
                let needed_bottom = self
                    .viewport_top
                    .saturating_add(rows)
                    .saturating_add(next_viewport_height);
                let scroll_by = needed_bottom.saturating_sub(self.screen_size.height);
                if scroll_by > 0 {
                    execute!(self.terminal.backend_mut(), ScrollUp(scroll_by))?;
                    self.viewport_top = self.viewport_top.saturating_sub(scroll_by);
                }
                self.draw_buffer_rows(&buffer, source_y, rows, self.viewport_top)?;
                self.viewport_top = self.viewport_top.saturating_add(rows);
                source_y = source_y.saturating_add(rows);
            }
        }
        Backend::flush(self.terminal.backend_mut())?;
        self.prepare_viewport(next_viewport_height)?;
        // Scrolling and drawing history bypass Ratatui's frame buffers, so its previous frame no
        // longer describes the physical viewport. Clear only the mutable tail and reset that
        // baseline so the immediately following draw repaints unchanged composer and footer cells
        // as well as changed content.
        execute!(
            self.terminal.backend_mut(),
            MoveTo(0, self.viewport_top),
            Clear(ClearType::FromCursorDown),
        )?;
        self.terminal.swap_buffers();
        Ok(())
    }

    pub(super) fn draw(
        &mut self,
        height: u16,
        draw: impl FnOnce(&mut ratatui::Frame<'_>),
    ) -> Result<()> {
        self.prepare_viewport(height)?;
        self.terminal
            .draw(draw)
            .context("failed to draw inline terminal UI")?;
        Ok(())
    }

    fn prepare_viewport(&mut self, height: u16) -> Result<()> {
        self.refresh_screen_size()?;
        let height = height.clamp(1, self.screen_size.height.max(1));
        let overflow = self
            .viewport_top
            .saturating_add(height)
            .saturating_sub(self.screen_size.height);
        if overflow > 0 {
            execute!(self.terminal.backend_mut(), ScrollUp(overflow))?;
            self.viewport_top = self.viewport_top.saturating_sub(overflow);
        }
        let area = Rect::new(0, self.viewport_top, self.screen_size.width, height);
        if area != self.viewport_area {
            if area.bottom() < self.viewport_area.bottom() {
                execute!(
                    self.terminal.backend_mut(),
                    MoveTo(0, area.bottom()),
                    Clear(ClearType::FromCursorDown),
                )?;
            }
            self.terminal
                .resize(area)
                .context("failed to resize inline terminal viewport")?;
            self.viewport_area = area;
        }
        Ok(())
    }

    fn refresh_screen_size(&mut self) -> Result<()> {
        let size = self
            .terminal
            .backend_mut()
            .size()
            .context("failed to read terminal size")?;
        if size != self.screen_size {
            let was_bottom_aligned = self.viewport_area.bottom() == self.screen_size.height;
            if was_bottom_aligned && size.height > self.screen_size.height {
                self.viewport_top = self
                    .viewport_top
                    .saturating_add(size.height - self.screen_size.height);
            }
            self.viewport_top = self.viewport_top.min(size.height.saturating_sub(1));
            self.screen_size = size;
        }
        Ok(())
    }

    fn draw_buffer_rows(
        &mut self,
        buffer: &Buffer,
        source_y: u16,
        rows: u16,
        destination_y: u16,
    ) -> Result<()> {
        let width = buffer.area.width;
        for row in 0..rows {
            let source_y = source_y.saturating_add(row);
            let destination_y = destination_y.saturating_add(row);
            let background = buffer[(width.saturating_sub(1), source_y)]
                .bg
                .into_crossterm();
            queue!(
                self.terminal.backend_mut(),
                MoveTo(0, destination_y),
                SetBackgroundColor(background),
                Clear(ClearType::CurrentLine),
                ResetColor,
                SetAttribute(Attribute::Reset),
            )?;

            // The terminal itself will soft-wrap a row written through its final column. Codex
            // clears the styled row first, then writes only visible content; doing the same keeps
            // resize from turning right-padding into blank continuation rows.
            let content_width = visible_row_width(buffer, source_y);
            self.terminal
                .backend_mut()
                .draw((0..content_width).map(|x| (x, destination_y, &buffer[(x, source_y)])))
                .context("failed to write transcript history")?;
        }
        Ok(())
    }

    fn finish(&mut self) {
        let _ = execute!(
            self.terminal.backend_mut(),
            MoveTo(0, self.viewport_top),
            Clear(ClearType::FromCursorDown),
        );
        let _ = self.terminal.show_cursor();
    }
}

/// Render each logical history line separately so its line-level style fills every physical row.
///
/// `Paragraph::new(Text::from(lines))` applies each `Line`'s style only to its graphemes. That is
/// normally invisible, but user-message lines carry the background for the full-width message
/// cell. Once trailing spaces are omitted from terminal output, treating all lines as one
/// paragraph collapses that background to the text itself. Codex's scrollback writer clears each
/// row with the `Line` style before writing its spans; using that style as the per-line paragraph
/// style preserves the same behavior while retaining Ratatui's wrapping here.
pub(super) fn render_history_lines(lines: &[Line<'static>], width: u16) -> Buffer {
    let width = width.max(1);
    let rendered_height = lines
        .iter()
        .map(|line| {
            Paragraph::new(line.clone())
                .wrap(Wrap { trim: false })
                .line_count(width)
                .max(1)
        })
        .fold(0, usize::saturating_add)
        .clamp(1, MAX_HISTORY_ROWS_PER_CELL);
    let rendered_height = u16::try_from(rendered_height).unwrap_or(u16::MAX);
    let area = Rect::new(0, 0, width, rendered_height);
    let mut buffer = Buffer::empty(area);
    let mut y = 0;

    for line in lines {
        if y >= rendered_height {
            break;
        }
        let paragraph = Paragraph::new(line.clone())
            .style(line.style)
            .wrap(Wrap { trim: false });
        let line_height = u16::try_from(paragraph.line_count(width).max(1)).unwrap_or(u16::MAX);
        let height = line_height.min(rendered_height.saturating_sub(y));
        paragraph.render(Rect::new(0, y, width, height), &mut buffer);
        y = y.saturating_add(height);
    }

    buffer
}

fn visible_row_width(buffer: &Buffer, y: u16) -> u16 {
    (0..buffer.area.width)
        .rfind(|&x| buffer[(x, y)].symbol() != " ")
        .map_or(0, |x| x.saturating_add(1))
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        self.terminal.finish();
        let _ = restore();
    }
}

fn restore() -> io::Result<()> {
    let raw_result = disable_raw_mode();
    let mode_result = execute!(
        stdout(),
        SetCursorStyle::DefaultUserShape,
        DisableBracketedPaste,
        PopKeyboardEnhancementFlags,
    );
    raw_result.and(mode_result)
}

fn install_panic_hook() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |panic| {
            let _ = restore();
            previous(panic);
        }));
    });
}

#[derive(Default)]
struct StartupProbe {
    foreground: Option<(u8, u8, u8)>,
    background: Option<(u8, u8, u8)>,
}

fn startup_probe(timeout: Duration) -> io::Result<StartupProbe> {
    let mut tty = ProbeTty::open()?;
    tty.write_all(b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\")?;
    let deadline = Instant::now() + timeout;
    let mut bytes = Vec::new();
    loop {
        tty.read_available(&mut bytes)?;
        let probe = StartupProbe {
            foreground: parse_osc_color(&bytes, 10),
            background: parse_osc_color(&bytes, 11),
        };
        if probe.foreground.is_some() && probe.background.is_some() {
            return Ok(probe);
        }
        let now = Instant::now();
        if now >= deadline || !tty.poll_readable(deadline.saturating_duration_since(now))? {
            return Ok(probe);
        }
    }
}

struct ProbeTty {
    reader: File,
    writer: File,
    original_flags: libc::c_int,
}

impl ProbeTty {
    fn open() -> io::Result<Self> {
        let stdio_reader = duplicate_file(libc::STDIN_FILENO);
        let stdio_writer = duplicate_file(libc::STDOUT_FILENO);
        let (reader, writer) = match (stdio_reader, stdio_writer) {
            (Ok(reader), Ok(writer)) => (reader, writer),
            _ => (
                OpenOptions::new().read(true).open("/dev/tty")?,
                OpenOptions::new().write(true).open("/dev/tty")?,
            ),
        };
        let fd = reader.as_raw_fd();
        let original_flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if original_flags == -1 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { libc::fcntl(fd, libc::F_SETFL, original_flags | libc::O_NONBLOCK) } == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            reader,
            writer,
            original_flags,
        })
    }

    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.writer.write_all(bytes)?;
        self.writer.flush()
    }

    fn read_available(&mut self, buffer: &mut Vec<u8>) -> io::Result<()> {
        let mut chunk = [0_u8; 256];
        loop {
            let read = unsafe {
                libc::read(
                    self.reader.as_raw_fd(),
                    chunk.as_mut_ptr().cast::<libc::c_void>(),
                    chunk.len(),
                )
            };
            if read > 0 {
                buffer.extend_from_slice(&chunk[..read as usize]);
                continue;
            }
            if read == 0 {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) {
                return Ok(());
            }
            return Err(error);
        }
    }

    fn poll_readable(&self, timeout: Duration) -> io::Result<bool> {
        let mut descriptor = libc::pollfd {
            fd: self.reader.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let timeout_ms = timeout.as_millis().min(libc::c_int::MAX as u128) as libc::c_int;
        loop {
            let result = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
            if result >= 0 {
                return Ok(result > 0 && descriptor.revents & libc::POLLIN != 0);
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
}

impl Drop for ProbeTty {
    fn drop(&mut self) {
        let _ = unsafe { libc::fcntl(self.reader.as_raw_fd(), libc::F_SETFL, self.original_flags) };
    }
}

fn duplicate_file(fd: libc::c_int) -> io::Result<File> {
    let duplicated = unsafe { libc::dup(fd) };
    if duplicated == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(duplicated) })
}

fn parse_osc_color(bytes: &[u8], slot: u8) -> Option<(u8, u8, u8)> {
    let prefix = format!("\x1b]{slot};");
    let start = bytes
        .windows(prefix.len())
        .position(|window| window == prefix.as_bytes())?;
    let remaining = &bytes[start + prefix.len()..];
    let end = remaining.iter().enumerate().find_map(|(index, byte)| {
        (*byte == 0x07 || (*byte == 0x1b && remaining.get(index + 1) == Some(&b'\\')))
            .then_some(index)
    })?;
    parse_osc_rgb(std::str::from_utf8(&remaining[..end]).ok()?)
}

fn parse_osc_rgb(value: &str) -> Option<(u8, u8, u8)> {
    let (kind, values) = value.trim().split_once(':')?;
    if !kind.eq_ignore_ascii_case("rgb") && !kind.eq_ignore_ascii_case("rgba") {
        return None;
    }
    let mut components = values.split('/');
    let red = parse_osc_component(components.next()?)?;
    let green = parse_osc_component(components.next()?)?;
    let blue = parse_osc_component(components.next()?)?;
    if kind.eq_ignore_ascii_case("rgba") {
        parse_osc_component(components.next()?)?;
    }
    components.next().is_none().then_some((red, green, blue))
}

fn parse_osc_component(value: &str) -> Option<u8> {
    match value.len() {
        2 => u8::from_str_radix(value, 16).ok(),
        4 => u16::from_str_radix(value, 16)
            .ok()
            .map(|component| (component / 257) as u8),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MODEL;
    use crate::events::AgentEvent;
    use crate::tui::view::View;
    use ratatui::backend::ClearType as BackendClearType;
    use ratatui::backend::WindowSize;
    use ratatui::buffer::Cell;
    use ratatui::layout::Position;
    use std::cell::RefCell;
    use std::path::Path;
    use std::rc::Rc;

    const TEST_SCROLLBACK_ROWS: usize = 512;

    #[derive(Clone)]
    struct SharedParser {
        parser: Rc<RefCell<vt100::Parser>>,
    }

    impl SharedParser {
        fn new(width: u16, height: u16) -> Self {
            Self {
                parser: Rc::new(RefCell::new(vt100::Parser::new(
                    height,
                    width,
                    TEST_SCROLLBACK_ROWS,
                ))),
            }
        }

        fn screen(&self) -> String {
            self.parser.borrow().screen().contents()
        }

        fn history_contains(&self, needle: &str) -> bool {
            let mut parser = self.parser.borrow_mut();
            let mut found = false;
            for offset in 1..=TEST_SCROLLBACK_ROWS {
                parser.screen_mut().set_scrollback(offset);
                found |= parser.screen().contents().contains(needle);
                if found || parser.screen().scrollback() < offset {
                    break;
                }
            }
            parser.screen_mut().set_scrollback(0);
            found
        }
    }

    impl Write for SharedParser {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.parser.borrow_mut().write(buffer)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.parser.borrow_mut().flush()
        }
    }

    struct VtBackend {
        backend: CrosstermBackend<SharedParser>,
        output: SharedParser,
        size: Size,
    }

    impl VtBackend {
        fn new(width: u16, height: u16) -> (Self, SharedParser) {
            let output = SharedParser::new(width, height);
            (
                Self {
                    backend: CrosstermBackend::new(output.clone()),
                    output: output.clone(),
                    size: Size::new(width, height),
                },
                output,
            )
        }
    }

    impl Write for VtBackend {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.backend.write(buffer)
        }

        fn flush(&mut self) -> io::Result<()> {
            Write::flush(&mut self.backend)
        }
    }

    impl Backend for VtBackend {
        type Error = io::Error;

        fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
        where
            I: Iterator<Item = (u16, u16, &'a Cell)>,
        {
            self.backend.draw(content)
        }

        fn hide_cursor(&mut self) -> io::Result<()> {
            self.backend.hide_cursor()
        }

        fn show_cursor(&mut self) -> io::Result<()> {
            self.backend.show_cursor()
        }

        fn get_cursor_position(&mut self) -> io::Result<Position> {
            Ok(self
                .output
                .parser
                .borrow()
                .screen()
                .cursor_position()
                .into())
        }

        fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
            self.backend.set_cursor_position(position)
        }

        fn clear(&mut self) -> io::Result<()> {
            self.backend.clear()
        }

        fn clear_region(&mut self, clear_type: BackendClearType) -> io::Result<()> {
            self.backend.clear_region(clear_type)
        }

        fn append_lines(&mut self, line_count: u16) -> io::Result<()> {
            self.backend.append_lines(line_count)
        }

        fn size(&self) -> io::Result<Size> {
            Ok(self.size)
        }

        fn window_size(&mut self) -> io::Result<WindowSize> {
            Ok(WindowSize {
                columns_rows: self.size,
                pixels: Size::new(640, 480),
            })
        }

        fn flush(&mut self) -> io::Result<()> {
            Write::flush(self)
        }
    }

    fn test_terminal(
        width: u16,
        screen_height: u16,
        viewport_height: u16,
    ) -> (AppTerminal<VtBackend>, SharedParser) {
        let viewport_top = screen_height - viewport_height;
        let viewport_area = Rect::new(0, viewport_top, width, viewport_height);
        let (backend, output) = VtBackend::new(width, screen_height);
        let terminal = Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Fixed(viewport_area),
            },
        )
        .unwrap();
        (
            AppTerminal {
                terminal,
                viewport_area,
                screen_size: Size::new(width, screen_height),
                viewport_top,
            },
            output,
        )
    }

    #[test]
    fn history_scroll_repaints_unchanged_composer_and_footer() {
        const WIDTH: u16 = 60;
        const SCREEN_HEIGHT: u16 = 12;

        let mut view = View::new(Path::new("/tmp/bettercodex"));
        let _ = view.take_pending_history_lines(WIDTH);
        view.start_turn("test the viewport");
        let _ = view.take_pending_history_lines(WIDTH);

        let prepared = view.prepare(WIDTH, SCREEN_HEIGHT);
        let viewport_height = prepared.height();
        let (mut terminal, output) = test_terminal(WIDTH, SCREEN_HEIGHT, viewport_height);
        terminal
            .draw(viewport_height, |frame| {
                view.render_prepared(frame, prepared);
            })
            .unwrap();
        let initial = output.screen();
        assert!(initial.contains('›'), "{initial}");
        assert!(initial.contains(MODEL), "{initial}");

        let history = (0..viewport_height)
            .map(|row| Line::from(format!("history row {row}")))
            .collect();
        terminal
            .insert_history_lines(history, viewport_height)
            .unwrap();
        view.handle_agent_event(AgentEvent::ReasoningSummarySectionStarted);
        view.handle_agent_event(AgentEvent::ReasoningSummaryDelta(
            "**Inspecting viewport state**".to_string(),
        ));
        let prepared = view.prepare(WIDTH, SCREEN_HEIGHT);
        terminal
            .draw(prepared.height(), |frame| {
                view.render_prepared(frame, prepared);
            })
            .unwrap();

        let repainted = output.screen();
        assert!(repainted.contains("history row"), "{repainted}");
        assert!(
            repainted.contains("Inspecting viewport state ("),
            "{repainted}"
        );
        assert!(repainted.contains(" • esc to interrupt)"), "{repainted}");
        assert!(repainted.contains('›'), "{repainted}");
        assert!(repainted.contains(MODEL), "{repainted}");
        let parser = output.parser.borrow();
        let screen = parser.screen();
        let activity_row = (0..SCREEN_HEIGHT)
            .find(|&row| {
                (0..WIDTH)
                    .filter_map(|column| screen.cell(row, column))
                    .map(vt100::Cell::contents)
                    .collect::<String>()
                    .contains("Inspecting viewport state")
            })
            .expect("rendered activity row");
        assert!(
            (0..WIDTH).all(|column| screen
                .cell(activity_row, column)
                .is_some_and(|cell| cell.bgcolor() == vt100::Color::Default)),
            "activity row retained a stale background"
        );
    }

    #[test]
    fn oversized_live_response_enters_terminal_scrollback_while_streaming() {
        const WIDTH: u16 = 48;
        const SCREEN_HEIGHT: u16 = 12;

        let mut view = View::new(Path::new("/tmp/bettercodex"));
        let mut history = view.take_pending_history_lines(WIDTH);
        view.start_turn("stream beyond the viewport");
        history.extend(view.take_pending_history_lines(WIDTH));
        let prepared = view.prepare(WIDTH, SCREEN_HEIGHT);
        let viewport_height = prepared.height();
        let (mut terminal, output) = test_terminal(WIDTH, SCREEN_HEIGHT, viewport_height);
        terminal
            .insert_history_lines(history, viewport_height)
            .unwrap();
        terminal
            .draw(viewport_height, |frame| {
                view.render_prepared(frame, prepared);
            })
            .unwrap();

        view.handle_agent_event(AgentEvent::ModelMessageDelta(
            (0..40)
                .map(|index| format!("- stream row {index}\n"))
                .collect(),
        ));
        let mut prepared = view.prepare(WIDTH, SCREEN_HEIGHT);
        let streamed_history = prepared.take_history_lines();
        assert!(
            streamed_history.iter().any(|line| line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
                .ends_with("stream row 0")),
            "{streamed_history:?}"
        );
        let viewport_height = prepared.height();
        terminal
            .insert_history_lines(streamed_history, viewport_height)
            .unwrap();
        terminal
            .draw(viewport_height, |frame| {
                view.render_prepared(frame, prepared);
            })
            .unwrap();

        let current = output.screen();
        assert!(current.contains("stream row 39"), "{current}");
        assert!(current.contains(MODEL), "{current}");
        assert!(output.history_contains("stream row 0"));
    }

    #[test]
    fn startup_probe_parses_terminal_colors() {
        let bytes = b"\x1b]10;rgb:eeee/dddd/cccc\x07\x1b]11;rgb:1111/2222/3333\x07";
        assert_eq!(parse_osc_color(bytes, 10), Some((238, 221, 204)));
        assert_eq!(parse_osc_color(bytes, 11), Some((17, 34, 51)));
    }

    #[test]
    fn history_rows_do_not_emit_terminal_width_padding() {
        let area = Rect::new(0, 0, 12, 2);
        let mut buffer = Buffer::empty(area);
        buffer.set_string(0, 0, "hello", ratatui::style::Style::default());
        buffer.set_style(
            Rect::new(0, 1, 12, 1),
            ratatui::style::Style::default().bg(ratatui::style::Color::Blue),
        );

        assert_eq!(visible_row_width(&buffer, 0), 5);
        assert_eq!(visible_row_width(&buffer, 1), 0);
    }

    #[test]
    fn history_rows_preserve_full_width_line_backgrounds() {
        let style = ratatui::style::Style::default().bg(ratatui::style::Color::Rgb(58, 58, 58));
        let lines = vec![
            Line::default().style(style),
            Line::from("› test").style(style),
            Line::default().style(style),
            Line::from("plain"),
        ];

        let buffer = render_history_lines(&lines, 12);

        for y in 0..3 {
            for x in 0..12 {
                assert_eq!(buffer[(x, y)].bg, style.bg.unwrap());
            }
        }
        for x in 0..12 {
            assert_eq!(buffer[(x, 3)].bg, ratatui::style::Color::Reset);
        }
        assert_eq!(visible_row_width(&buffer, 0), 0);
        assert_eq!(visible_row_width(&buffer, 1), 6);
        assert_eq!(visible_row_width(&buffer, 2), 0);
    }
}
