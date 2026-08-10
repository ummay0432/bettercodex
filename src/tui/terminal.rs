use super::terminal_hyperlinks;
use super::terminal_hyperlinks::HyperlinkLine;
use crate::managed_session::PreparedTmuxSession;
use crate::managed_session::WorkerHandoff;
use anyhow::Context;
use anyhow::Result;
use crossterm::cursor::MoveTo;
use crossterm::cursor::SetCursorStyle;
use crossterm::event::DisableBracketedPaste;
use crossterm::event::DisableFocusChange;
use crossterm::event::EnableBracketedPaste;
#[cfg(unix)]
use crossterm::event::EnableFocusChange;
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
#[cfg(test)]
use ratatui::text::Line;
use ratatui::widgets::Clear as WidgetClear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use ratatui::widgets::Wrap;
#[cfg(unix)]
use std::fs::File;
#[cfg(unix)]
use std::fs::OpenOptions;
use std::io;
use std::io::Stdout;
use std::io::Write;
use std::io::stdout;
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::fd::FromRawFd;
use std::sync::Once;
#[cfg(unix)]
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

#[cfg(unix)]
const STARTUP_PROBE_TIMEOUT: Duration = Duration::from_millis(100);
#[cfg(windows)]
const STARTUP_PROBE_TIMEOUT: () = ();
const MAX_HISTORY_ROWS_PER_CELL: usize = 30_000;
const CLEAR_VISIBLE_TERMINAL: &[u8] = b"\x1b[r\x1b[0m\x1b[H\x1b[2J\x1b[H";
const LINE_FEED_CHUNK: [u8; 256] = [b'\n'; 256];

/// Codex keeps finalized transcript cells in normal terminal scrollback and owns only the mutable
/// tail plus composer. Ratatui's fixed viewport gives this lean client the same dynamic-height
/// ownership without pulling in Codex's alternate-screen overlays.
pub(super) struct AppTerminal<B: Backend = CrosstermBackend<Stdout>> {
    terminal: Terminal<B>,
    viewport_area: Rect,
    screen_size: Size,
    viewport_top: u16,
}

pub(super) struct TerminalStartup {
    probe: Option<PendingStartupProbe>,
    restore_on_drop: bool,
}

pub(super) struct TerminalSession {
    terminal: AppTerminal,
    default_foreground: Option<(u8, u8, u8)>,
    default_background: Option<(u8, u8, u8)>,
}

impl TerminalStartup {
    pub(super) fn begin() -> Result<Self> {
        install_panic_hook();
        initialize_terminal_modes()?;
        // Start Codex's terminal-color query immediately so its bounded wait overlaps agent setup.
        Ok(Self {
            probe: PendingStartupProbe::begin(STARTUP_PROBE_TIMEOUT).ok(),
            restore_on_drop: true,
        })
    }

    pub(super) fn enter(mut self) -> Result<TerminalSession> {
        ensure_virtual_terminal_processing()?;
        let mut output = stdout();

        let probe = self.finish_probe();
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

        let session = TerminalSession {
            terminal: AppTerminal {
                terminal,
                viewport_area,
                screen_size,
                viewport_top: 0,
            },
            default_foreground: probe.foreground,
            default_background: probe.background,
        };
        self.restore_on_drop = false;
        Ok(session)
    }

    fn finish_probe(&mut self) -> StartupProbe {
        self.probe
            .take()
            .and_then(|probe| probe.finish().ok())
            .unwrap_or_default()
    }
}

impl TerminalSession {
    pub(super) fn terminal_mut(&mut self) -> &mut AppTerminal {
        &mut self.terminal
    }

    pub(super) fn default_background(&self) -> Option<(u8, u8, u8)> {
        self.default_background
    }

    pub(super) fn default_foreground(&self) -> Option<(u8, u8, u8)> {
        self.default_foreground
    }

    pub(super) fn migrate_to_tmux(
        &mut self,
        prepared: PreparedTmuxSession,
        supervisor: &mut WorkerHandoff,
    ) -> Result<String> {
        supervisor.transfer(&prepared)?;
        Ok(prepared.commit())
    }
}

impl Drop for TerminalStartup {
    fn drop(&mut self) {
        if self.restore_on_drop {
            // Keep raw mode active until any OSC replies emitted by the startup probe have been
            // consumed. Otherwise a fallible setup step can return those replies to the shell as
            // visible input after terminal restoration.
            let _ = self.finish_probe();
            let _ = restore();
        }
    }
}

impl<B> AppTerminal<B>
where
    B: Backend<Error = io::Error> + Write,
{
    pub(super) fn clear_screen(&mut self) -> Result<()> {
        ensure_virtual_terminal_processing()?;
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
        lines: Vec<HyperlinkLine>,
        next_viewport_height: u16,
    ) -> Result<()> {
        if lines.is_empty() {
            return Ok(());
        }
        ensure_virtual_terminal_processing()?;
        self.refresh_screen_size()?;
        let width = self.screen_size.width.max(1);
        let next_viewport_height = next_viewport_height.clamp(1, self.screen_size.height.max(1));
        let buffer = render_history_lines(&lines, width);
        let rendered_height = buffer.area.height;

        let history_capacity = self.screen_size.height.saturating_sub(next_viewport_height);
        if history_capacity == 0 {
            for source_y in 0..rendered_height {
                self.draw_buffer_rows(&buffer, source_y, 1, 0)?;
                self.scroll_screen_up_into_scrollback(1)?;
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
                    self.scroll_screen_up_into_scrollback(scroll_by)?;
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
        ensure_virtual_terminal_processing()?;
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
            self.scroll_screen_up_into_scrollback(overflow)?;
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

    /// Scroll the complete normal screen with line feeds so displaced rows enter scrollback.
    ///
    /// `CSI S` only edits the active page in terminals such as Windows Terminal and xterm.js, so
    /// rows pushed above the screen can be discarded instead of becoming scrollback. A line feed
    /// at the bottom of the full scrolling region uses the same history-producing path as ordinary
    /// shell output. The mutable viewport is repainted after every caller of this helper.
    fn scroll_screen_up_into_scrollback(&mut self, rows: u16) -> Result<()> {
        if rows == 0 || self.screen_size.height == 0 {
            return Ok(());
        }

        let writer = self.terminal.backend_mut();
        writer.write_all(b"\x1b[r\x1b[0m")?;
        queue!(writer, MoveTo(0, self.screen_size.height - 1))?;
        let mut remaining = usize::from(rows);
        while remaining > 0 {
            let chunk_len = remaining.min(LINE_FEED_CHUNK.len());
            writer.write_all(&LINE_FEED_CHUNK[..chunk_len])?;
            remaining -= chunk_len;
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
        let _ = ensure_virtual_terminal_processing();
        let _ = execute!(
            self.terminal.backend_mut(),
            MoveTo(0, self.viewport_top),
            Clear(ClearType::FromCursorDown),
        );
        let _ = self.terminal.show_cursor();
    }
}

/// Render transcript lines into a viewport while extending each logical line's style across every
/// physical row it occupies.
///
/// A multi-line `Paragraph` applies each `Line` style only to that line's graphemes. That collapses
/// backgrounds to the text width in the live viewport, while the terminal scrollback writer clears
/// the whole row with the same style. Render each visible logical-line slice as its own styled
/// paragraph so live and committed transcript cells match without adding copy-visible padding.
pub(super) fn render_transcript_lines(
    lines: &[HyperlinkLine],
    area: Rect,
    scroll_rows: usize,
    buffer: &mut Buffer,
) {
    if area.is_empty() {
        return;
    }

    WidgetClear.render(area, buffer);
    let viewport_end = scroll_rows.saturating_add(usize::from(area.height));
    let mut line_start = 0_usize;
    for line in lines {
        let paragraph = Paragraph::new(line.line.clone())
            .style(line.line.style)
            .wrap(Wrap { trim: false });
        let line_height = transcript_line_height(line, area.width);
        let line_end = line_start.saturating_add(line_height);
        let visible_start = line_start.max(scroll_rows);
        let visible_end = line_end.min(viewport_end);
        if visible_start < visible_end {
            let y = area.y.saturating_add(
                u16::try_from(visible_start.saturating_sub(scroll_rows)).unwrap_or(u16::MAX),
            );
            let height = u16::try_from(visible_end - visible_start).unwrap_or(u16::MAX);
            paragraph
                .scroll((
                    u16::try_from(visible_start.saturating_sub(line_start)).unwrap_or(u16::MAX),
                    0,
                ))
                .render(Rect::new(area.x, y, area.width, height), buffer);
        }
        line_start = line_end;
        if line_start >= viewport_end {
            break;
        }
    }
    terminal_hyperlinks::mark_buffer_hyperlinks(buffer, area, lines, scroll_rows);
}

/// Number of physical terminal rows occupied by one transcript line at `width`.
pub(super) fn transcript_line_height(line: &HyperlinkLine, width: u16) -> usize {
    Paragraph::new(line.line.clone())
        .wrap(Wrap { trim: false })
        .line_count(width.max(1))
        .max(1)
}

/// Render finalized transcript lines for insertion into terminal scrollback.
pub(super) fn render_history_lines(lines: &[HyperlinkLine], width: u16) -> Buffer {
    let width = width.max(1);
    let rendered_height = lines
        .iter()
        .map(|line| transcript_line_height(line, width))
        .fold(0, usize::saturating_add)
        .clamp(1, MAX_HISTORY_ROWS_PER_CELL);
    let rendered_height = u16::try_from(rendered_height).unwrap_or(u16::MAX);
    let area = Rect::new(0, 0, width, rendered_height);
    let mut buffer = Buffer::empty(area);
    render_transcript_lines(lines, area, /*scroll_rows*/ 0, &mut buffer);

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

fn initialize_terminal_modes() -> Result<()> {
    ensure_virtual_terminal_processing().context("failed to enable terminal VT output")?;
    enable_raw_mode().context("failed to enable terminal raw mode")?;
    #[cfg(windows)]
    if let Err(error) = super::windows_console::set_input_record_mode() {
        let _ = restore();
        return Err(error).context("failed to configure Windows console input records");
    }

    #[cfg(unix)]
    if let Err(error) = execute!(
        stdout(),
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
        ),
        EnableBracketedPaste,
        EnableFocusChange,
        SetCursorStyle::SteadyBar,
    ) {
        let _ = restore();
        return Err(error).context("failed to initialize terminal input modes");
    }

    #[cfg(windows)]
    {
        if let Err(error) = execute!(stdout(), EnableBracketedPaste, SetCursorStyle::SteadyBar) {
            let _ = restore();
            return Err(error).context("failed to initialize terminal input modes");
        }
        // Legacy Windows consoles do not implement the Kitty keyboard protocol.
        // Crossterm input records still provide the keys bettercodex needs.
        let _ = execute!(
            stdout(),
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
            ),
            DisableFocusChange,
        );
    }
    Ok(())
}

#[cfg(unix)]
fn restore() -> io::Result<()> {
    let raw_result = disable_raw_mode();
    let mode_result = execute!(
        stdout(),
        SetCursorStyle::DefaultUserShape,
        DisableBracketedPaste,
        DisableFocusChange,
        PopKeyboardEnhancementFlags,
    );
    raw_result.and(mode_result)
}

#[cfg(windows)]
fn restore() -> io::Result<()> {
    let mut first_error = ensure_virtual_terminal_processing().err();
    if let Err(error) = disable_raw_mode() {
        first_error.get_or_insert(error);
    }
    if let Err(error) = execute!(
        stdout(),
        SetCursorStyle::DefaultUserShape,
        DisableBracketedPaste,
        DisableFocusChange,
    ) {
        first_error.get_or_insert(error);
    }
    // Keyboard enhancement is best-effort on Windows, so restoration is too.
    let _ = execute!(stdout(), PopKeyboardEnhancementFlags);
    if let Err(error) = super::windows_console::restore_input_mode() {
        first_error.get_or_insert(error);
    }
    first_error.map_or(Ok(()), Err)
}

/// Reassert ANSI output processing because another console client can change the shared screen
/// buffer mode while bettercodex is running. Redirected handles are deliberately ignored.
#[cfg(windows)]
fn ensure_virtual_terminal_processing() -> io::Result<()> {
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Console::ENABLE_PROCESSED_OUTPUT;
    use windows_sys::Win32::System::Console::ENABLE_VIRTUAL_TERMINAL_PROCESSING;
    use windows_sys::Win32::System::Console::GetConsoleMode;
    use windows_sys::Win32::System::Console::GetStdHandle;
    use windows_sys::Win32::System::Console::STD_ERROR_HANDLE;
    use windows_sys::Win32::System::Console::STD_OUTPUT_HANDLE;
    use windows_sys::Win32::System::Console::SetConsoleMode;

    fn enable_for_handle(handle: HANDLE) -> io::Result<()> {
        if handle == INVALID_HANDLE_VALUE || handle == 0 {
            return Ok(());
        }

        let mut mode = 0;
        if unsafe { GetConsoleMode(handle, &mut mode) } == 0 {
            return Ok(());
        }

        let requested = ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING;
        if mode & requested == requested {
            return Ok(());
        }
        if unsafe { SetConsoleMode(handle, mode | requested) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    enable_for_handle(unsafe { GetStdHandle(STD_OUTPUT_HANDLE) })?;
    enable_for_handle(unsafe { GetStdHandle(STD_ERROR_HANDLE) })
}

#[cfg(not(windows))]
fn ensure_virtual_terminal_processing() -> io::Result<()> {
    Ok(())
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

#[cfg(unix)]
struct PendingStartupProbe {
    tty: ProbeTty,
    deadline: Instant,
    bytes: Vec<u8>,
}

#[cfg(unix)]
impl PendingStartupProbe {
    fn begin(timeout: Duration) -> io::Result<Self> {
        let mut tty = ProbeTty::open()?;
        tty.write_all(b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\")?;
        Ok(Self {
            tty,
            deadline: Instant::now() + timeout,
            bytes: Vec::new(),
        })
    }

    fn finish(mut self) -> io::Result<StartupProbe> {
        loop {
            self.tty.read_available(&mut self.bytes)?;
            let probe = StartupProbe {
                foreground: parse_osc_color(&self.bytes, 10),
                background: parse_osc_color(&self.bytes, 11),
            };
            if probe.foreground.is_some() && probe.background.is_some() {
                return Ok(probe);
            }
            let now = Instant::now();
            if now >= self.deadline
                || !self
                    .tty
                    .poll_readable(self.deadline.saturating_duration_since(now))?
            {
                return Ok(probe);
            }
        }
    }
}

#[cfg(unix)]
struct ProbeTty {
    reader: File,
    writer: File,
    original_flags: libc::c_int,
}

#[cfg(unix)]
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

#[cfg(unix)]
impl Drop for ProbeTty {
    fn drop(&mut self) {
        let _ = unsafe { libc::fcntl(self.reader.as_raw_fd(), libc::F_SETFL, self.original_flags) };
    }
}

#[cfg(unix)]
fn duplicate_file(fd: libc::c_int) -> io::Result<File> {
    let duplicated = unsafe { libc::dup(fd) };
    if duplicated == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(duplicated) })
}

#[cfg(windows)]
struct PendingStartupProbe;

#[cfg(windows)]
impl PendingStartupProbe {
    fn begin(_timeout: ()) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "terminal color probe is unavailable on Windows",
        ))
    }

    fn finish(self) -> io::Result<StartupProbe> {
        Ok(StartupProbe::default())
    }
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
    use crate::events::AgentEvent;
    use crate::model::DEFAULT_MODEL as MODEL;
    use crate::tui::view::View;
    use ratatui::backend::ClearType as BackendClearType;
    use ratatui::backend::WindowSize;
    use ratatui::buffer::Cell;
    use ratatui::layout::Position;
    use serde_json::json;
    use std::cell::RefCell;
    use std::path::Path;
    use std::rc::Rc;
    use std::time::Duration;

    const TEST_SCROLLBACK_ROWS: usize = 512;

    #[derive(Clone)]
    struct SharedParser {
        parser: Rc<RefCell<vt100::Parser>>,
        bytes: Rc<RefCell<Vec<u8>>>,
    }

    impl SharedParser {
        fn new(width: u16, height: u16) -> Self {
            Self {
                parser: Rc::new(RefCell::new(vt100::Parser::new(
                    height,
                    width,
                    TEST_SCROLLBACK_ROWS,
                ))),
                bytes: Rc::new(RefCell::new(Vec::new())),
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

        fn output_contains(&self, sequence: &[u8]) -> bool {
            self.bytes
                .borrow()
                .windows(sequence.len())
                .any(|window| window == sequence)
        }

        fn emitted_csi_scroll_up(&self) -> bool {
            let bytes = self.bytes.borrow();
            bytes.iter().enumerate().any(|(index, byte)| {
                if *byte != 0x1b || bytes.get(index + 1) != Some(&b'[') {
                    return false;
                }
                let mut end = index + 2;
                while bytes.get(end).is_some_and(u8::is_ascii_digit) {
                    end += 1;
                }
                end > index + 2 && bytes.get(end) == Some(&b'S')
            })
        }
    }

    impl Write for SharedParser {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.bytes.borrow_mut().extend_from_slice(buffer);
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

    fn redraw_test_terminal(
        view: &mut View,
        terminal: &mut AppTerminal<VtBackend>,
        width: u16,
        screen_height: u16,
    ) {
        let history = view.take_pending_history_lines(width, screen_height);
        render_test_terminal(view, terminal, width, screen_height, history);
    }

    fn reflow_test_terminal(
        view: &mut View,
        terminal: &mut AppTerminal<VtBackend>,
        width: u16,
        screen_height: u16,
    ) {
        terminal.clear_screen().unwrap();
        let history = view.history_lines_for_resize_reflow(width, screen_height);
        render_test_terminal(view, terminal, width, screen_height, history);
    }

    fn render_test_terminal(
        view: &mut View,
        terminal: &mut AppTerminal<VtBackend>,
        width: u16,
        screen_height: u16,
        mut history: Vec<HyperlinkLine>,
    ) {
        let mut prepared = view.prepare(width, screen_height);
        history.extend(prepared.take_history_lines());
        let viewport_height = prepared.height();
        terminal
            .insert_history_lines(history, viewport_height)
            .unwrap();
        terminal
            .draw(viewport_height, |frame| {
                view.render_prepared(frame, prepared);
            })
            .unwrap();
    }

    #[test]
    fn incremental_live_response_preserves_preceding_tool_and_stream_prefix() {
        const WIDTH: u16 = 48;
        const SCREEN_HEIGHT: u16 = 12;

        let mut view = View::new(Path::new("/tmp/bettercodex"));
        let (mut terminal, output) =
            test_terminal(WIDTH, SCREEN_HEIGHT, /*viewport_height*/ 1);
        redraw_test_terminal(&mut view, &mut terminal, WIDTH, SCREEN_HEIGHT);
        view.start_turn("stream beyond the viewport after a tool");
        redraw_test_terminal(&mut view, &mut terminal, WIDTH, SCREEN_HEIGHT);
        view.handle_agent_event(AgentEvent::ToolStarted {
            call_id: "tool-before-stream".to_string(),
            name: "exec_command".to_string(),
            input: Some(json!({"cmd": "printf tool-marker"})),
        });
        view.handle_agent_event(AgentEvent::ToolCompleted {
            call_id: "tool-before-stream".to_string(),
            output: Ok(json!({"exit_code": 0, "output": "tool-marker\n"})),
            duration: Duration::from_millis(1),
        });

        for index in 0..40 {
            view.handle_agent_event(AgentEvent::ModelMessageDelta(format!(
                "response-row-{index:02}\n"
            )));
            redraw_test_terminal(&mut view, &mut terminal, WIDTH, SCREEN_HEIGHT);
        }

        let screen = output.screen();
        assert!(screen.contains("response-row-39"), "{screen}");
        assert!(screen.contains(MODEL), "{screen}");
        assert!(output.history_contains("Ran printf tool-marker"));
        assert!(output.history_contains("response-row-00"));

        reflow_test_terminal(&mut view, &mut terminal, WIDTH, SCREEN_HEIGHT);

        let screen = output.screen();
        assert!(screen.contains("response-row-39"), "{screen}");
        assert!(screen.contains(MODEL), "{screen}");
        assert!(output.history_contains("Ran printf tool-marker"));
        assert!(output.history_contains("response-row-00"));
        assert!(!output.emitted_csi_scroll_up());
    }

    #[test]
    fn overwide_live_response_line_enters_scrollback_without_cropping() {
        const WIDTH: u16 = 48;
        const SCREEN_HEIGHT: u16 = 12;

        let mut view = View::new(Path::new("/tmp/bettercodex"));
        let (mut terminal, output) =
            test_terminal(WIDTH, SCREEN_HEIGHT, /*viewport_height*/ 1);
        redraw_test_terminal(&mut view, &mut terminal, WIDTH, SCREEN_HEIGHT);
        view.start_turn("stream an overwide code line followed by a live tail");
        redraw_test_terminal(&mut view, &mut terminal, WIDTH, SCREEN_HEIGHT);
        view.handle_agent_event(AgentEvent::ToolStarted {
            call_id: "tool-before-wrapped-stream".to_string(),
            name: "exec_command".to_string(),
            input: Some(json!({"cmd": "printf wrapped-tool-marker"})),
        });
        view.handle_agent_event(AgentEvent::ToolCompleted {
            call_id: "tool-before-wrapped-stream".to_string(),
            output: Ok(json!({"exit_code": 0, "output": "wrapped-tool-marker\n"})),
            duration: Duration::from_millis(1),
        });

        let response = format!(
            "```text\nprefix-marker-{}\nstream-tail-marker",
            "x".repeat(800)
        );
        for chunk in response.as_bytes().chunks(37) {
            view.handle_agent_event(AgentEvent::ModelMessageDelta(
                std::str::from_utf8(chunk).unwrap().to_string(),
            ));
            redraw_test_terminal(&mut view, &mut terminal, WIDTH, SCREEN_HEIGHT);
        }

        let screen = output.screen();
        assert!(screen.contains("stream-tail-marker"), "{screen}");
        assert!(screen.contains(MODEL), "{screen}");
        assert!(output.history_contains("Ran printf wrapped-tool-marker"));
        assert!(output.history_contains("prefix-marker"));
    }

    #[test]
    fn resize_replay_rebuilds_scrollback_with_full_screen_line_feeds() {
        const WIDTH: u16 = 24;
        const SCREEN_HEIGHT: u16 = 4;

        let (mut terminal, output) =
            test_terminal(WIDTH, SCREEN_HEIGHT, /*viewport_height*/ 1);
        terminal.clear_screen().unwrap();
        let lines = (0..8)
            .map(|index| Line::from(format!("history-row-{index:02}")))
            .collect();
        let lines = terminal_hyperlinks::plain_hyperlink_lines(lines);

        terminal.insert_history_lines(lines, 1).unwrap();

        assert!(output.history_contains("history-row-00"));
        assert!(!output.emitted_csi_scroll_up());
        assert!(output.output_contains(b"\x1b[r\x1b[0m\x1b[4;1H\n"));
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

        let lines = terminal_hyperlinks::plain_hyperlink_lines(lines);
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

    #[test]
    fn transcript_line_backgrounds_follow_wrapping_and_scroll() {
        let first_style = ratatui::style::Style::default().bg(ratatui::style::Color::Blue);
        let second_style = ratatui::style::Style::default().bg(ratatui::style::Color::Red);
        let lines = terminal_hyperlinks::plain_hyperlink_lines(vec![
            Line::from("abcdefghijkl").style(first_style),
            Line::from("tail").style(second_style),
        ]);
        let area = Rect::new(0, 0, 6, 2);
        let mut buffer = Buffer::empty(area);

        render_transcript_lines(&lines, area, /*scroll_rows*/ 1, &mut buffer);

        assert_eq!(visible_row_width(&buffer, 0), 6);
        assert_eq!(visible_row_width(&buffer, 1), 4);
        for x in 0..area.width {
            assert_eq!(buffer[(x, 0)].bg, first_style.bg.unwrap());
            assert_eq!(buffer[(x, 1)].bg, second_style.bg.unwrap());
        }
    }
}
