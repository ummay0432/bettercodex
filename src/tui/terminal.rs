mod startup_replay;

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
use crossterm::terminal::BeginSynchronizedUpdate;
use crossterm::terminal::Clear;
use crossterm::terminal::ClearType;
use crossterm::terminal::EndSynchronizedUpdate;
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
use ratatui::widgets::Clear as WidgetClear;
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
const MAX_STARTUP_PROBE_BYTES: usize = 64 * 1024;
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

    pub(super) fn screen_size(&self) -> Size {
        self.screen_size
    }

    /// Keep destructive viewport updates and their replacement frame in one terminal update.
    ///
    /// Growing an inline response can resize Ratatui's fixed viewport or move completed rows into
    /// scrollback. Both operations clear or scroll visible cells before the next draw restores the
    /// mutable tail. Terminals that implement synchronized updates continue presenting the prior
    /// frame until the entire transaction is ready, avoiding a visible blank intermediate frame.
    pub(super) fn synchronized_update<T>(
        &mut self,
        update: impl FnOnce(&mut Self) -> Result<T>,
    ) -> Result<T> {
        queue!(self.terminal.backend_mut(), BeginSynchronizedUpdate)
            .context("failed to begin synchronized terminal update")?;
        let update_result = update(self);
        // Always attempt to release the terminal's presentation lock, including when a clear,
        // history insertion, or draw failed.
        let end_result = execute!(self.terminal.backend_mut(), EndSynchronizedUpdate)
            .context("failed to end synchronized terminal update");
        let value = update_result?;
        end_result?;
        Ok(value)
    }

    /// Update geometry from one size resolved for the whole frame.
    ///
    /// Ordinary redraws reuse this cached value. Resize, focus, and maintenance boundaries sample
    /// the backend so coalesced or missed notifications cannot leave it stale.
    fn set_screen_size(&mut self, size: Size) -> bool {
        if size == self.screen_size {
            return false;
        }
        let was_bottom_aligned = self.viewport_area.bottom() == self.screen_size.height;
        if was_bottom_aligned && size.height > self.screen_size.height {
            self.viewport_top = self
                .viewport_top
                .saturating_add(size.height - self.screen_size.height);
        }
        self.viewport_top = self.viewport_top.min(size.height.saturating_sub(1));
        self.screen_size = size;
        true
    }

    /// Reconcile cached geometry with the terminal after a resize settles or the display surface
    /// may have changed. Returns whether the cached size changed.
    pub(super) fn refresh_screen_size(&mut self) -> Result<bool> {
        let size = self
            .terminal
            .backend_mut()
            .size()
            .context("failed to read terminal size")?;
        Ok(self.set_screen_size(size))
    }

    pub(super) fn insert_history_lines(
        &mut self,
        lines: Vec<HyperlinkLine>,
        next_viewport_height: u16,
    ) -> Result<()> {
        if lines.is_empty() {
            return Ok(());
        }
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
        self.prepare_viewport(height)?;
        self.terminal
            .draw(draw)
            .context("failed to draw inline terminal UI")?;
        Ok(())
    }

    fn prepare_viewport(&mut self, height: u16) -> Result<()> {
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
    /// `CSI S` only edits the active page in terminal emulators such as xterm.js, so
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
    enable_raw_mode().context("failed to enable terminal raw mode")?;

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

    Ok(())
}

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

struct PendingStartupProbe {
    tty: ProbeTty,
    deadline: Instant,
    bytes: Vec<u8>,
}

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
        let result = self.read();
        crossterm::event::buffer_input(&startup_replay::startup_replay_input(&self.bytes))?;
        result
    }

    fn read(&mut self) -> io::Result<StartupProbe> {
        loop {
            self.tty
                .read_available(&mut self.bytes, self.deadline, MAX_STARTUP_PROBE_BYTES)?;
            let probe = StartupProbe {
                foreground: parse_osc_color(&self.bytes, 10),
                background: parse_osc_color(&self.bytes, 11),
            };
            if probe.foreground.is_some() && probe.background.is_some() {
                return Ok(probe);
            }
            let now = Instant::now();
            if now >= self.deadline
                || self.bytes.len() >= MAX_STARTUP_PROBE_BYTES
                || !self
                    .tty
                    .poll_readable(self.deadline.saturating_duration_since(now))?
            {
                return Ok(probe);
            }
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
        Self::new(reader, writer)
    }

    fn new(reader: File, writer: File) -> io::Result<Self> {
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

    fn read_available(
        &mut self,
        buffer: &mut Vec<u8>,
        deadline: Instant,
        max_bytes: usize,
    ) -> io::Result<()> {
        let mut chunk = [0_u8; 256];
        loop {
            if buffer.len() >= max_bytes || Instant::now() >= deadline {
                return Ok(());
            }
            let bytes_to_read = chunk.len().min(max_bytes.saturating_sub(buffer.len()));
            let read = unsafe {
                libc::read(
                    self.reader.as_raw_fd(),
                    chunk.as_mut_ptr().cast::<libc::c_void>(),
                    bytes_to_read,
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
        let deadline = Instant::now() + timeout;
        loop {
            let now = Instant::now();
            if now >= deadline {
                return Ok(false);
            }
            let timeout_ms = deadline
                .saturating_duration_since(now)
                .as_millis()
                .min(libc::c_int::MAX as u128) as libc::c_int;
            let result = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
            if result > 0 {
                return Ok(descriptor.revents & libc::POLLIN != 0);
            }
            if result == 0 {
                return Ok(false);
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
    let (end, _) = osc_payload_end(remaining)?;
    parse_osc_rgb(std::str::from_utf8(&remaining[..end]).ok()?)
}

fn osc_payload_end(bytes: &[u8]) -> Option<(usize, usize)> {
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            0x07 => return Some((index, 1)),
            0x1b if bytes.get(index + 1) == Some(&b'\\') => return Some((index, 2)),
            _ => index += 1,
        }
    }
    None
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
