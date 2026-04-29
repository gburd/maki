use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::text_buffer::{EditResult, TextBuffer};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViMode {
    #[default]
    Normal,
    Insert,
}

impl ViMode {
    pub fn indicator(&self) -> &'static str {
        match self {
            Self::Normal => "NORMAL",
            Self::Insert => "INSERT",
        }
    }
}

#[derive(Debug, Default)]
pub struct ViState {
    pub mode: ViMode,
    pending_op: Option<ViOperator>,
    yank_buffer: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViOperator {
    Delete,
    Yank,
}

impl ViState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Handle a key in Vi mode. Returns the edit result.
    pub fn handle_key(&mut self, buf: &mut TextBuffer, key: KeyEvent) -> EditResult {
        match self.mode {
            ViMode::Insert => self.handle_insert(buf, key),
            ViMode::Normal => self.handle_normal(buf, key),
        }
    }

    fn handle_insert(&mut self, buf: &mut TextBuffer, key: KeyEvent) -> EditResult {
        match key.code {
            KeyCode::Esc => {
                self.mode = ViMode::Normal;
                // Move cursor back one if possible (vi behavior)
                if buf.x() > 0 {
                    buf.move_left();
                }
                EditResult::Moved
            }
            _ => buf.handle_key(key),
        }
    }

    fn handle_normal(&mut self, buf: &mut TextBuffer, key: KeyEvent) -> EditResult {
        let m = key.modifiers;
        // In normal mode, ignore modifiers for most keys
        if m.contains(KeyModifiers::CONTROL) || m.contains(KeyModifiers::ALT) {
            return EditResult::Ignored;
        }

        if let Some(op) = self.pending_op {
            return self.handle_pending_operator(buf, op, key);
        }

        match key.code {
            // Mode switching
            KeyCode::Char('i') => {
                self.mode = ViMode::Insert;
                EditResult::Moved
            }
            KeyCode::Char('a') => {
                self.mode = ViMode::Insert;
                buf.move_right();
                EditResult::Moved
            }
            KeyCode::Char('A') => {
                self.mode = ViMode::Insert;
                buf.move_end();
                EditResult::Moved
            }
            KeyCode::Char('I') => {
                self.mode = ViMode::Insert;
                buf.move_home();
                EditResult::Moved
            }
            KeyCode::Char('o') => {
                self.mode = ViMode::Insert;
                buf.move_end();
                buf.add_line();
                EditResult::Changed
            }
            KeyCode::Char('O') => {
                self.mode = ViMode::Insert;
                buf.move_home();
                buf.add_line();
                buf.move_up();
                EditResult::Changed
            }

            // Movement
            KeyCode::Char('h') | KeyCode::Left => {
                buf.move_left();
                EditResult::Moved
            }
            KeyCode::Char('l') | KeyCode::Right => {
                buf.move_right();
                EditResult::Moved
            }
            KeyCode::Char('k') | KeyCode::Up => {
                buf.move_up();
                EditResult::Moved
            }
            KeyCode::Char('j') | KeyCode::Down => {
                buf.move_down();
                EditResult::Moved
            }
            KeyCode::Char('w') => {
                buf.move_word_right();
                EditResult::Moved
            }
            KeyCode::Char('b') => {
                buf.move_word_left();
                EditResult::Moved
            }
            KeyCode::Char('e') => {
                // Move to end of word (similar to word right for now)
                buf.move_word_right();
                EditResult::Moved
            }
            KeyCode::Char('0') | KeyCode::Home => {
                buf.move_home();
                EditResult::Moved
            }
            KeyCode::Char('$') | KeyCode::End => {
                buf.move_end();
                EditResult::Moved
            }

            // Delete
            KeyCode::Char('x') => {
                buf.delete_char();
                EditResult::Changed
            }
            KeyCode::Char('d') => {
                self.pending_op = Some(ViOperator::Delete);
                EditResult::Ignored
            }
            KeyCode::Char('y') => {
                self.pending_op = Some(ViOperator::Yank);
                EditResult::Ignored
            }

            // Paste
            KeyCode::Char('p') => {
                if !self.yank_buffer.is_empty() {
                    buf.insert_text(&self.yank_buffer);
                    EditResult::Changed
                } else {
                    EditResult::Ignored
                }
            }

            // Other
            KeyCode::Esc => {
                self.pending_op = None;
                EditResult::Ignored
            }

            _ => EditResult::Ignored,
        }
    }

    fn handle_pending_operator(
        &mut self,
        buf: &mut TextBuffer,
        op: ViOperator,
        key: KeyEvent,
    ) -> EditResult {
        self.pending_op = None;

        match key.code {
            // dd = delete line, yy = yank line
            KeyCode::Char('d') if op == ViOperator::Delete => {
                let line = buf.lines()[buf.y()].clone();
                self.yank_buffer = line;
                // Delete the current line
                if buf.line_count() == 1 {
                    buf.move_home();
                    buf.kill_to_end_of_line();
                } else {
                    // Remove current line by selecting it all
                    buf.move_home();
                    buf.kill_to_end_of_line();
                    if buf.y() < buf.line_count() - 1 {
                        buf.delete_char(); // merge with next
                    } else if buf.y() > 0 {
                        // Last line: merge with previous by removing char at start
                        buf.remove_char();
                    }
                }
                EditResult::Changed
            }
            KeyCode::Char('y') if op == ViOperator::Yank => {
                self.yank_buffer = buf.lines()[buf.y()].clone();
                EditResult::Ignored
            }
            KeyCode::Char('w') => {
                let start_x = buf.x();
                let start_y = buf.y();
                buf.move_word_right();
                let end_x = buf.x();
                let end_y = buf.y();
                // Only handle same-line operations for simplicity
                if start_y == end_y {
                    let line = &buf.lines()[start_y];
                    let byte_start = TextBuffer::char_to_byte(line, start_x);
                    let byte_end = TextBuffer::char_to_byte(line, end_x);
                    let yanked = line[byte_start..byte_end].to_string();
                    // Move cursor back
                    buf.move_word_left();
                    match op {
                        ViOperator::Delete => {
                            buf.delete_word_after_cursor();
                            self.yank_buffer = yanked;
                            EditResult::Changed
                        }
                        ViOperator::Yank => {
                            self.yank_buffer = yanked;
                            EditResult::Ignored
                        }
                    }
                } else {
                    // Cross-line: just move back for now
                    while buf.y() != start_y {
                        buf.move_up();
                    }
                    while buf.x() != start_x {
                        buf.move_left();
                    }
                    EditResult::Ignored
                }
            }
            KeyCode::Char('b') => {
                let start_x = buf.x();
                let start_y = buf.y();
                buf.move_word_left();
                let end_x = buf.x();
                let end_y = buf.y();
                if start_y == end_y {
                    let line = &buf.lines()[start_y];
                    let byte_start = TextBuffer::char_to_byte(line, end_x);
                    let byte_end = TextBuffer::char_to_byte(line, start_x);
                    let yanked = line[byte_start..byte_end].to_string();
                    match op {
                        ViOperator::Delete => {
                            buf.remove_word_before_cursor();
                            self.yank_buffer = yanked;
                            EditResult::Changed
                        }
                        ViOperator::Yank => {
                            self.yank_buffer = yanked;
                            // Move back to original position
                            buf.move_word_right();
                            EditResult::Ignored
                        }
                    }
                } else {
                    // Cross-line: move back
                    while buf.y() != start_y {
                        buf.move_down();
                    }
                    while buf.x() != start_x {
                        buf.move_right();
                    }
                    EditResult::Ignored
                }
            }
            KeyCode::Char('$') => match op {
                ViOperator::Delete => {
                    let line = &buf.lines()[buf.y()];
                    let bx = TextBuffer::char_to_byte(line, buf.x());
                    self.yank_buffer = line[bx..].to_string();
                    buf.kill_to_end_of_line();
                    EditResult::Changed
                }
                ViOperator::Yank => {
                    let line = &buf.lines()[buf.y()];
                    let bx = TextBuffer::char_to_byte(line, buf.x());
                    self.yank_buffer = line[bx..].to_string();
                    EditResult::Ignored
                }
            },
            _ => EditResult::Ignored,
        }
    }
}
