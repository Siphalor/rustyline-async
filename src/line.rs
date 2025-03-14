use std::io::{self, Write};

use crossterm::{
	cursor,
	event::{Event, KeyCode, KeyEvent, KeyModifiers},
	terminal::{Clear, ClearType::*},
	QueueableCommand,
};

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::{History, ReadlineError};

#[derive(Default)]
pub struct LineState {
	// Unicode Line
	line: String,
	// Index of grapheme in line
	line_cursor_grapheme: usize,
	// Column of grapheme in line
	current_column: u16,

	cluster_buffer: String, // buffer for holding partial grapheme clusters as they come in

	prompt: String,
	last_line_length: usize,
	last_line_completed: bool,

	term_size: (u16, u16),

	pub history: History,
}

impl LineState {
	pub fn new(prompt: String, term_size: (u16, u16)) -> Self {
		let current_column = prompt.len() as u16;
		Self {
			prompt,
			last_line_completed: true,
			term_size,
			current_column,

			..Default::default()
		}
	}
	fn line_height(&self, pos: u16) -> u16 {
		pos / self.term_size.0 // Gets the number of lines wrapped
	}
	/// Move from a position on the line to the start
	fn move_to_beginning(&self, term: &mut impl Write, from: u16) -> io::Result<()> {
		let move_up = self.line_height(from.saturating_sub(1));
		term.queue(cursor::MoveToColumn(1))?
			.queue(cursor::MoveUp(move_up))?;
		Ok(())
	}
	/// Move from the start of the line to some position
	fn move_from_beginning(&self, term: &mut impl Write, to: u16) -> io::Result<()> {
		let line_height = self.line_height(to.saturating_sub(1));
		let line_remaining_len = to % self.term_size.0; // Get the remaining length
		term.queue(cursor::MoveDown(line_height))?
			.queue(cursor::MoveRight(line_remaining_len))?;
		Ok(())
	}
	fn move_cursor(&mut self, change: isize) -> io::Result<()> {
		// self.reset_cursor(term)?;
		if change > 0 {
			let count = self.line.graphemes(true).count();
			self.line_cursor_grapheme =
				usize::min(self.line_cursor_grapheme as usize + change as usize, count);
		} else {
			self.line_cursor_grapheme =
				self.line_cursor_grapheme.saturating_sub((-change) as usize);
		}
		let (pos, str) = self.current_grapheme().unwrap_or((0, ""));
		let pos = pos + str.len();
		self.current_column =
			(self.prompt.len() + UnicodeWidthStr::width(&self.line[0..pos])) as u16;

		// self.set_cursor(term)?;

		Ok(())
	}
	fn current_grapheme(&self) -> Option<(usize, &str)> {
		self.line
			.grapheme_indices(true)
			.take(self.line_cursor_grapheme)
			.last()
	}
	fn reset_cursor(&self, term: &mut impl Write) -> io::Result<()> {
		self.move_to_beginning(term, self.current_column)
	}
	fn set_cursor(&self, term: &mut impl Write) -> io::Result<()> {
		self.move_from_beginning(term, self.current_column as u16)
	}
	/// Clear current line
	fn clear(&self, term: &mut impl Write) -> io::Result<()> {
		self.move_to_beginning(term, self.current_column as u16)?;
		term.queue(Clear(FromCursorDown))?;
		Ok(())
	}
	/// Render line
	pub fn render(&self, term: &mut impl Write) -> io::Result<()> {
		write!(term, "{}{}", self.prompt, self.line)?;
		let line_len = self.prompt.len() + UnicodeWidthStr::width(&self.line[..]);
		self.move_to_beginning(term, line_len as u16)?;
		self.move_from_beginning(term, self.current_column)?;
		Ok(())
	}
	/// Clear line and render
	pub fn clear_and_render(&self, term: &mut impl Write) -> io::Result<()> {
		self.clear(term)?;
		self.render(term)?;
		Ok(())
	}
	pub fn print_data(&mut self, data: &[u8], term: &mut impl Write) -> Result<(), ReadlineError> {
		self.clear(term)?;

		// If last written data was not newline, restore the cursor
		if !self.last_line_completed {
			term.queue(cursor::MoveUp(1))?
				.queue(cursor::MoveToColumn(1))?
				.queue(cursor::MoveRight(self.last_line_length as u16))?;
		}

		// Write data in a way that newlines also act as carriage returns
		for line in data.split_inclusive(|b| *b == b'\n') {
			term.write_all(line)?;
			term.queue(cursor::MoveToColumn(1))?;
		}

		self.last_line_completed = data.ends_with(b"\n"); // Set whether data ends with newline

		// If data does not end with newline, save the cursor and write newline for prompt
		// Usually data does end in newline due to the buffering of SharedWriter, but sometimes it may not (i.e. if .flush() is called)
		if !self.last_line_completed {
			self.last_line_length += data.len();
			// Make sure that last_line_length wraps around when doing multiple writes
			if self.last_line_length >= self.term_size.0 as usize {
				self.last_line_length %= self.term_size.0 as usize;
				writeln!(term)?;
			}
			writeln!(term)?; // Move to beginning of line and make new line
		} else {
			self.last_line_length = 0;
		}

		term.queue(cursor::MoveToColumn(1))?;

		self.render(term)?;
		Ok(())
	}
	pub fn print(&mut self, string: &str, term: &mut impl Write) -> Result<(), ReadlineError> {
		self.print_data(string.as_bytes(), term)?;
		Ok(())
	}
	pub async fn handle_event(
		&mut self,
		event: Event,
		term: &mut impl Write,
	) -> Result<Option<String>, ReadlineError> {
		// Update history entries
		self.history.update().await;

		match event {
			// Regular Modifiers (None or Shift)
			Event::Key(KeyEvent {
				code,
				modifiers: KeyModifiers::NONE,
			})
			| Event::Key(KeyEvent {
				code,
				modifiers: KeyModifiers::SHIFT,
			}) => match code {
				KeyCode::Enter => {
					self.clear(term)?;
					let line = std::mem::take(&mut self.line);
					self.move_cursor(-100000)?;
					self.render(term)?;

					return Ok(Some(line));
				}
				// Delete character from line
				KeyCode::Backspace => {
					if let Some((pos, str)) = self.current_grapheme() {
						self.clear(term)?;

						let len = pos + str.len();
						self.line.replace_range(pos..len, "");
						self.move_cursor(-1)?;

						self.render(term)?;
					}
				}
				KeyCode::Left => {
					self.reset_cursor(term)?;
					self.move_cursor(-1)?;
					self.set_cursor(term)?;
				}
				KeyCode::Right => {
					self.reset_cursor(term)?;
					self.move_cursor(1)?;
					self.set_cursor(term)?;
				}
				KeyCode::Home => {
					self.reset_cursor(term)?;
					self.move_cursor(-100000)?;
					self.set_cursor(term)?;
				}
				KeyCode::End => {
					self.reset_cursor(term)?;
					self.move_cursor(100000)?;
					self.set_cursor(term)?;
				}
				KeyCode::Up => {
					// search for next history item, replace line if found.
					if let Some(line) = self.history.search_next(&self.line) {
						self.line.clear();
						self.line += line;
						self.clear(term)?;
						self.move_cursor(100000)?;
						self.render(term)?;
					}
				}
				KeyCode::Down => {
					// search for next history item, replace line if found.
					if let Some(line) = self.history.search_previous(&self.line) {
						self.line.clear();
						self.line += line;
						self.clear(term)?;
						self.move_cursor(100000)?;
						self.render(term)?;
					}
				}
				// Add character to line and output
				KeyCode::Char(c) => {
					self.clear(term)?;
					let prev_len = self.cluster_buffer.graphemes(true).count();
					self.cluster_buffer.push(c);
					let new_len = self.cluster_buffer.graphemes(true).count();

					let (g_pos, g_str) = self.current_grapheme().unwrap_or((0, ""));
					let pos = g_pos + g_str.len();

					self.line.insert(pos, c);

					if prev_len != new_len {
						self.move_cursor(1)?;
						if prev_len > 0 {
							if let Some((pos, str)) =
								self.cluster_buffer.grapheme_indices(true).next()
							{
								let len = str.len();
								self.cluster_buffer.replace_range(pos..len, "");
							}
						}
					}
					self.render(term)?;
				}
				_ => {}
			},
			// Control Keys
			Event::Key(KeyEvent {
				code,
				modifiers: KeyModifiers::CONTROL,
			}) => match code {
				// End of transmission (CTRL-D)
				KeyCode::Char('d') => {
					writeln!(term)?;
					self.clear(term)?;
					return Err(ReadlineError::Eof);
				}
				// End of text (CTRL-C)
				KeyCode::Char('c') => {
					self.print(&format!("{}{}", self.prompt, self.line), term)?;
					self.line.clear();
					self.move_cursor(-10000)?;
					self.clear_and_render(term)?;
					return Err(ReadlineError::Interrupted);
				}
				// Clear all
				KeyCode::Char('l') => {
					term.queue(Clear(All))?.queue(cursor::MoveTo(0, 0))?;
					self.clear_and_render(term)?;
				}
				// Clear to start
				KeyCode::Char('u') => {
					if let Some((pos, str)) = self.current_grapheme() {
						let pos = pos + str.len();
						self.line.drain(0..pos);
						self.move_cursor(-10000)?;
						self.clear_and_render(term)?;
					}
				}
				// Move to beginning
				#[cfg(feature = "emacs")]
				KeyCode::Char('a') => {
					self.reset_cursor(term)?;
					self.move_cursor(-100000)?;
					self.set_cursor(term)?;
				}
				// Move to end
				#[cfg(feature = "emacs")]
				KeyCode::Char('e') => {
					self.reset_cursor(term)?;
					self.move_cursor(100000)?;
					self.set_cursor(term)?;
				}
				// Move cursor left to previous word
				KeyCode::Left => {
					self.reset_cursor(term)?;
					let count = self.line.graphemes(true).count();
					let skip_count = count - self.line_cursor_grapheme;
					if let Some((pos, _)) = self
						.line
						.grapheme_indices(true)
						.rev()
						.skip(skip_count)
						.skip_while(|(_, str)| *str == " ")
						.find(|(_, str)| *str == " ")
					{
						let change = pos as isize - self.line_cursor_grapheme as isize;
						self.move_cursor(change + 1)?;
					} else {
						self.move_cursor(-10000)?
					}
					self.set_cursor(term)?;
				}
				// Move cursor right to next word
				KeyCode::Right => {
					self.reset_cursor(term)?;
					if let Some((pos, _)) = self
						.line
						.grapheme_indices(true)
						.skip(self.line_cursor_grapheme)
						.skip_while(|(_, c)| *c == " ")
						.find(|(_, c)| *c == " ")
					{
						let change = pos as isize - self.line_cursor_grapheme as isize;
						self.move_cursor(change)?;
					} else {
						self.move_cursor(10000)?;
					};
					self.set_cursor(term)?;
				}
				_ => {}
			},
			Event::Resize(x, y) => {
				self.term_size = (x, y);
				self.clear_and_render(term)?;
			}
			_ => {}
		}
		Ok(None)
	}
}
