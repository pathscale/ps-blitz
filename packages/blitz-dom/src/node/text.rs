use blitz_traits::{
    events::{BlitzImeEvent, BlitzKeyEvent},
    shell::ShellProvider,
};
use keyboard_types::{Code, Key, Modifiers};
use parley::{ContentWidths, FontContext, LayoutContext};

use crate::util::{ACTION_MOD, has_clipboard_modifier};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClipboardCommand {
    Copy,
    Cut,
    Paste,
}

fn clipboard_command(event: &BlitzKeyEvent) -> Option<ClipboardCommand> {
    if !has_clipboard_modifier(event.modifiers) {
        return None;
    }
    match event.code {
        Code::KeyC => Some(ClipboardCommand::Copy),
        Code::KeyX => Some(ClipboardCommand::Cut),
        Code::KeyV => Some(ClipboardCommand::Paste),
        _ => match &event.key {
            Key::Character(c) if c.eq_ignore_ascii_case("c") => Some(ClipboardCommand::Copy),
            Key::Character(c) if c.eq_ignore_ascii_case("x") => Some(ClipboardCommand::Cut),
            Key::Character(c) if c.eq_ignore_ascii_case("v") => Some(ClipboardCommand::Paste),
            _ => None,
        },
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
/// Parley Brush type for Blitz which contains the Blitz node id
pub struct TextBrush {
    /// The node id for the span
    pub id: usize,
}

impl TextBrush {
    pub(crate) fn from_id(id: usize) -> Self {
        Self { id }
    }
}

/// A [`ContentWidths`] result together with the inline box widths it was derived from.
///
/// Only ever produced by [`TextLayout::content_widths`]. Invalidation sites set
/// [`TextLayout::content_widths`] to `None` rather than constructing this.
#[derive(Clone, Debug)]
pub struct CachedContentWidths {
    /// The `width` of every inline box in the layout, as raw bit patterns, at the moment
    /// `widths` was computed. Stored as bits so the comparison is exact rather than
    /// approximate, and boxed so that the overwhelmingly common "no inline boxes" case does
    /// not allocate.
    inline_box_widths: Box<[u32]>,
    widths: ContentWidths,
}

#[derive(Clone, Default)]
pub struct TextLayout {
    pub text: String,
    pub content_widths: Option<CachedContentWidths>,
    pub layout: parley::layout::Layout<TextBrush>,
}

impl TextLayout {
    pub fn new() -> Self {
        Default::default()
    }

    /// The layout's min-content and max-content widths, recomputed only when the inputs to
    /// that computation have actually changed.
    ///
    /// WHY this is cached: `Layout::calculate_content_widths` walks every shaped cluster in
    /// the layout, and block layout asks for the content widths two or three times per pass
    /// (once under a min-content constraint, once under max-content, then again for the
    /// definite measure), so the same scan is repeated over the same data.
    ///
    /// WHY it is safe: the result is a pure function of exactly two things, the shaped runs
    /// and the current width of each inline box.
    ///
    /// The shaped runs only change when the inline layout is rebuilt, and every rebuild goes
    /// through `build_inline_layout_into`, which clears this cache. Damage propagation clears
    /// it too, in the same places it clears the Taffy layout cache, so a text edit or a style
    /// change affecting font, size, weight, letter/word spacing or white-space collapsing
    /// always re-measures.
    ///
    /// The inline box widths are the reason this cannot be a plain one-shot cache: they are
    /// re-measured on every pass, and an inline box legitimately measures differently under a
    /// min-content constraint than under a max-content one, so the same shaped text can yield
    /// different content widths from one call to the next. Rather than guess which constraint
    /// a cached entry belongs to, we record the box widths the entry was computed from and
    /// reuse it only when they are bit-for-bit identical. A layout containing no inline
    /// boxes, which is the common case and where the scan cost is concentrated, therefore
    /// hits the cache on every pass after the first.
    pub fn content_widths(&mut self) -> ContentWidths {
        // `InlineBox::kind` is fixed when the layout is built (and a change to it goes via a
        // rebuild, which invalidates this cache), so the widths alone identify the inline box
        // state that `calculate_content_widths` reads.
        let inline_box_widths: Box<[u32]> = self
            .layout
            .inline_boxes()
            .iter()
            .map(|ibox| ibox.width.to_bits())
            .collect();

        if let Some(cached) = &self.content_widths
            && cached.inline_box_widths == inline_box_widths
        {
            return cached.widths;
        }

        let widths = self.layout.calculate_content_widths();
        self.content_widths = Some(CachedContentWidths {
            inline_box_widths,
            widths,
        });
        widths
    }
}

impl std::fmt::Debug for TextLayout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TextLayout")
    }
}

// TODO: support keypress events
pub enum GeneratedTextInputEvent {
    Input,
    Select,
    PreEditChange,
    Submit,
}

pub struct TextInputData {
    /// A parley TextEditor instance
    pub editor: Box<parley::PlainEditor<TextBrush>>,
    /// Shaped placeholder text, painted only while the editable value is empty.
    pub placeholder_editor: Option<Box<parley::PlainEditor<TextBrush>>>,
    /// Whether the input is a singleline or multiline input
    pub is_multiline: bool,
    /// The scroll offset of the text content within the input, in CSS (unscaled) pixels.
    ///
    /// For single-line inputs this is a horizontal offset; for multi-line inputs it is a
    /// vertical offset. It is kept up to date so that the caret remains visible within the
    /// input's content box.
    pub scroll_offset: f32,
    pub layout_width: Option<f32>,
}

// FIXME: Implement Clone for PlainEditor
impl Clone for TextInputData {
    fn clone(&self) -> Self {
        TextInputData::new(self.is_multiline)
    }
}

impl TextInputData {
    pub fn new(is_multiline: bool) -> Self {
        let editor = Box::new(parley::PlainEditor::new(16.0));
        Self {
            editor,
            placeholder_editor: None,
            is_multiline,
            scroll_offset: 0.0,
            layout_width: None,
        }
    }

    pub fn sync_multiline_width(
        &mut self,
        font_ctx: &mut FontContext,
        layout_ctx: &mut LayoutContext<TextBrush>,
        width: f32,
    ) {
        if !self.is_multiline || width <= 0.0 {
            return;
        }
        if self
            .layout_width
            .is_some_and(|current| (current - width).abs() < 0.01)
        {
            return;
        }
        self.layout_width = Some(width);
        self.editor.set_width(Some(width));
        self.editor.driver(font_ctx, layout_ctx).refresh_layout();
        if let Some(placeholder) = self.placeholder_editor.as_mut() {
            placeholder.set_width(Some(width));
            placeholder.driver(font_ctx, layout_ctx).refresh_layout();
        }
    }

    pub fn set_text(
        &mut self,
        font_ctx: &mut FontContext,
        layout_ctx: &mut LayoutContext<TextBrush>,
        text: &str,
    ) {
        if self.editor.text() != text {
            self.editor.set_text(text);
            self.editor.driver(font_ctx, layout_ctx).refresh_layout();
        }
    }

    /// Recompute [`Self::scroll_offset`] so that the caret stays visible within the input's
    /// content box.
    ///
    /// `content_box_width` and `content_box_height` are the dimensions of the input's content
    /// box in CSS (unscaled) pixels.
    pub fn clamp_scroll_offset(&mut self, content_box_width: f32, content_box_height: f32) {
        let Some(layout) = self.editor.try_layout() else {
            return;
        };
        // Parley lays out at the editor's scale, so its geometry is in scaled (device) pixels.
        // We convert into CSS (unscaled) pixels to match `scroll_offset` and the content box.
        let scale = layout.scale();

        // The caret geometry relative to the start of the text content.
        let Some(caret) = self.editor.cursor_geometry(1.5) else {
            return;
        };

        // Caret bounds and content/viewport extents along the scrolling axis (CSS pixels).
        let (caret_start, caret_end, content, viewport) = if self.is_multiline {
            (
                caret.y0 as f32 / scale,
                caret.y1 as f32 / scale,
                layout.height() / scale,
                content_box_height,
            )
        } else {
            (
                caret.x0 as f32 / scale,
                caret.x1 as f32 / scale,
                layout.full_width() / scale,
                content_box_width,
            )
        };

        let mut offset = self.scroll_offset;

        // Scroll so that both edges of the caret are within the visible region.
        if caret_end > offset + viewport {
            offset = caret_end - viewport;
        }
        if caret_start < offset {
            offset = caret_start;
        }

        // Never scroll past the content, and never scroll into negative space. The content
        // extent includes the caret so that a caret at the very end remains fully visible
        // (its rendered width extends slightly past the text).
        let max_offset = (content.max(caret_end) - viewport).max(0.0);
        self.scroll_offset = offset.clamp(0.0, max_offset);
    }

    /// The maximum valid value of [`Self::scroll_offset`] (in CSS pixels) given the input's
    /// content box, i.e. the extent by which the text content overflows the content box along
    /// the input's scroll axis.
    ///
    /// `content_box_width` and `content_box_height` are the dimensions of the input's content
    /// box in CSS (unscaled) pixels.
    pub fn max_scroll_offset(&self, content_box_width: f32, content_box_height: f32) -> f32 {
        let Some(layout) = self.editor.try_layout() else {
            return 0.0;
        };
        let scale = layout.scale();
        let (content, viewport) = if self.is_multiline {
            (layout.height() / scale, content_box_height)
        } else {
            (layout.full_width() / scale, content_box_width)
        };
        (content - viewport).max(0.0)
    }

    /// Scroll the input's text content by `delta` CSS pixels along its scroll axis (horizontal
    /// for single-line inputs, vertical for multi-line inputs), clamping to the scrollable
    /// range.
    ///
    /// Returns the portion of `delta` that could not be consumed (because the input was already
    /// scrolled to its limit), so the caller can bubble it up to an ancestor scroller.
    pub fn scroll_by(
        &mut self,
        delta: f32,
        content_box_width: f32,
        content_box_height: f32,
    ) -> f32 {
        let max_offset = self.max_scroll_offset(content_box_width, content_box_height);
        if max_offset <= 0.0 {
            return delta;
        }

        // Match the sign convention used for block scrolling: a positive delta decreases the
        // scroll offset.
        let new_offset = (self.scroll_offset - delta).clamp(0.0, max_offset);
        let consumed = self.scroll_offset - new_offset;
        self.scroll_offset = new_offset;
        delta - consumed
    }

    pub(crate) fn apply_keypress_event(
        &mut self,
        font_ctx: &mut FontContext,
        layout_ctx: &mut LayoutContext<TextBrush>,
        shell_provider: &dyn ShellProvider,
        event: BlitzKeyEvent,
    ) -> Option<GeneratedTextInputEvent> {
        // Do nothing if it is a keyup event
        if !event.state.is_pressed() {
            return None;
        }

        let mods = event.modifiers;
        let shift = mods.contains(Modifiers::SHIFT);
        let action_mod = mods.contains(ACTION_MOD);
        let word_mod = mods.contains(Modifiers::ALT);
        let is_multiline = self.is_multiline;
        let editor = &mut self.editor;
        let mut driver = editor.driver(font_ctx, layout_ctx);
        if let Some(command) = clipboard_command(&event) {
            match command {
                ClipboardCommand::Copy => {
                    if let Some(text) = driver.editor.selected_text() {
                        let _ = shell_provider.set_clipboard_text(text.to_owned());
                    }
                }
                ClipboardCommand::Cut => {
                    if let Some(text) = driver.editor.selected_text() {
                        let _ = shell_provider.set_clipboard_text(text.to_owned());
                        driver.delete_selection()
                    }
                }
                ClipboardCommand::Paste => {
                    let text = shell_provider.get_clipboard_text().unwrap_or_default();
                    driver.insert_or_replace_selection(&text)
                }
            }

            return Some(GeneratedTextInputEvent::Input);
        }
        match event.key {
            Key::Character(c) if action_mod && matches!(c.to_lowercase().as_str(), "a") => {
                if shift {
                    driver.collapse_selection()
                } else {
                    driver.select_all()
                }
                return Some(GeneratedTextInputEvent::Select);
            }
            Key::ArrowLeft => {
                if action_mod {
                    if shift {
                        driver.select_to_line_start()
                    } else {
                        driver.move_to_line_start()
                    }
                } else if word_mod {
                    if shift {
                        driver.select_word_left()
                    } else {
                        driver.move_word_left()
                    }
                } else if shift {
                    driver.select_left()
                } else {
                    driver.move_left()
                }
                return Some(GeneratedTextInputEvent::Select);
            }
            Key::ArrowRight => {
                if action_mod {
                    if shift {
                        driver.select_to_line_end()
                    } else {
                        driver.move_to_line_end()
                    }
                } else if word_mod {
                    if shift {
                        driver.select_word_right()
                    } else {
                        driver.move_word_right()
                    }
                } else if shift {
                    driver.select_right()
                } else {
                    driver.move_right()
                }
                return Some(GeneratedTextInputEvent::Select);
            }
            Key::ArrowUp => {
                if action_mod && shift {
                    driver.select_to_text_start()
                } else if action_mod {
                    driver.move_to_text_start()
                } else if shift {
                    driver.select_up()
                } else {
                    driver.move_up()
                }
                return Some(GeneratedTextInputEvent::Select);
            }
            Key::ArrowDown => {
                if action_mod && shift {
                    driver.select_to_text_end()
                } else if action_mod {
                    driver.move_to_text_end()
                } else if shift {
                    driver.select_down()
                } else {
                    driver.move_down()
                }
                return Some(GeneratedTextInputEvent::Select);
            }
            Key::Home => {
                if action_mod {
                    if shift {
                        driver.select_to_text_start()
                    } else {
                        driver.move_to_text_start()
                    }
                } else if shift {
                    driver.select_to_line_start()
                } else {
                    driver.move_to_line_start()
                }
                return Some(GeneratedTextInputEvent::Select);
            }
            Key::End => {
                if action_mod {
                    if shift {
                        driver.select_to_text_end()
                    } else {
                        driver.move_to_text_end()
                    }
                } else if shift {
                    driver.select_to_line_end()
                } else {
                    driver.move_to_line_end()
                }
                return Some(GeneratedTextInputEvent::Select);
            }
            Key::Delete => {
                #[cfg(target_os = "macos")]
                if mods.contains(Modifiers::SUPER) {
                    if driver.editor.raw_selection().is_collapsed() {
                        driver.select_to_line_end();
                    }
                    driver.delete_selection();
                } else if mods.contains(Modifiers::ALT) {
                    driver.delete_word();
                } else {
                    driver.delete();
                }
                #[cfg(not(target_os = "macos"))]
                if action_mod {
                    driver.delete_word();
                } else {
                    driver.delete();
                }
                return Some(GeneratedTextInputEvent::Input);
            }
            Key::Backspace => {
                #[cfg(target_os = "macos")]
                if mods.contains(Modifiers::SUPER) {
                    if driver.editor.raw_selection().is_collapsed() {
                        driver.select_to_line_start();
                    }
                    driver.delete_selection();
                } else if mods.contains(Modifiers::ALT) {
                    driver.backdelete_word();
                } else {
                    driver.backdelete();
                }
                #[cfg(not(target_os = "macos"))]
                if action_mod {
                    driver.backdelete_word();
                } else {
                    driver.backdelete();
                }
                return Some(GeneratedTextInputEvent::Input);
            }

            Key::Character(c) if c == "\n" => {
                if is_multiline {
                    driver.insert_or_replace_selection("\n");
                    return Some(GeneratedTextInputEvent::Input);
                } else {
                    return Some(GeneratedTextInputEvent::Submit);
                }
            }
            Key::Enter => {
                if is_multiline {
                    driver.insert_or_replace_selection("\n");
                    return Some(GeneratedTextInputEvent::Input);
                } else {
                    return Some(GeneratedTextInputEvent::Submit);
                }
            }
            Key::Character(s)
                if !mods.contains(Modifiers::CONTROL) && !mods.contains(Modifiers::SUPER) =>
            {
                driver.insert_or_replace_selection(&s);
                return Some(GeneratedTextInputEvent::Input);
            }
            _ => {}
        };

        None
    }

    pub(crate) fn apply_apple_standard_keybinding(
        &mut self,
        font_ctx: &mut FontContext,
        layout_ctx: &mut LayoutContext<TextBrush>,
        shell_provider: &dyn ShellProvider,
        command: &str,
    ) -> Option<GeneratedTextInputEvent> {
        let editor = &mut self.editor;
        let mut driver = editor.driver(font_ctx, layout_ctx);
        let is_multiline = self.is_multiline;

        match command {
            // Inserting Content

            // Inserts a backtab character.
            "insertBacktab:" => {}
            // Inserts a container break, such as a new page break.
            "insertContainerBreak:" => {}
            // Inserts a double quotation mark without substituting a curly quotation mark.
            "insertDoubleQuoteIgnoringSubstitution:" => {
                driver.insert_or_replace_selection("\"");
                return Some(GeneratedTextInputEvent::Input);
            }
            // Inserts a line break character.
            "insertLineBreak:" => {
                driver.insert_or_replace_selection("\n");
                return Some(GeneratedTextInputEvent::Input);
            }
            // Inserts a newline character.
            "insertNewline:" => {
                if is_multiline {
                    driver.insert_or_replace_selection("\n");
                    return Some(GeneratedTextInputEvent::Input);
                } else {
                    return Some(GeneratedTextInputEvent::Submit);
                }
            }
            // Inserts a newline character without invoking the field editor’s normal handling to end editing.
            "insertNewlineIgnoringFieldEditor:" => {
                driver.insert_or_replace_selection("\n");
                return Some(GeneratedTextInputEvent::Input);
            }
            // Inserts a paragraph separator.
            "insertParagraphSeparator:" => {
                driver.insert_or_replace_selection("\n");
                return Some(GeneratedTextInputEvent::Input);
            }
            "insertSingleQuoteIgnoringSubstitution:" => {
                driver.insert_or_replace_selection("'");
                return Some(GeneratedTextInputEvent::Input);
            }
            // Inserts a tab character.
            "insertTab:" | "insertTabIgnoringFieldEditor:" => {
                // Ignore for now seeing as parley has poor support for laying out tabs
            }
            // Inserts the text you specify.
            "insertText:" => {}

            // Deleting Content

            // Deletes content moving backward from the current insertion point.
            // Physical Backspace/Delete events are handled directly above. AppKit may
            // deliver these selectors as well, but applying both would delete twice.
            "deleteBackward:" | "deleteBackwardByDecomposingPreviousCharacter:" => {}
            "deleteForward:" => {}
            // Deletes content from the insertion point to the beginning of the current line.
            "deleteToBeginningOfLine:" => {
                if driver.editor.raw_selection().is_collapsed() {
                    driver.select_to_line_start();
                }
                driver.delete_selection();
                return Some(GeneratedTextInputEvent::Input);
            }
            // Deletes content from the insertion point to the beginning of the current paragraph.
            "deleteToEndOfLine:" => {
                if driver.editor.raw_selection().is_collapsed() {
                    driver.select_to_line_end();
                }
                driver.delete_selection();
                return Some(GeneratedTextInputEvent::Input);
            }
            "deleteToBeginningOfParagraph:" => {
                if driver.editor.raw_selection().is_collapsed() {
                    driver.select_to_hard_line_start();
                }
                driver.delete_selection();
                return Some(GeneratedTextInputEvent::Input);
            }

            // Deletes content from the insertion point to the end of the current line.
            "deleteToEndOfParagraph:" => {
                if driver.editor.raw_selection().is_collapsed() {
                    driver.select_to_hard_line_end();
                }
                driver.delete_selection();
                return Some(GeneratedTextInputEvent::Input);
            }
            // Deletes content from the insertion point to the end of the current paragraph.
            "deleteWordBackward:" => {}
            // Deletes the word preceding the current insertion point.
            "deleteWordForward:" => {}
            // Deletes the current selection, placing it in a temporary buffer, such as the Clipboard.
            "yank:" => {
                if let Some(text) = driver.editor.selected_text() {
                    let _ = shell_provider.set_clipboard_text(text.to_owned());
                    driver.delete_selection();
                    return Some(GeneratedTextInputEvent::Input);
                }
            }

            // Moving the Insertion Pointer

            // Moves the insertion pointer backward in the current content.
            "moveBackward:" => {
                driver.move_left(); // TODO: Bidi-aware
                return Some(GeneratedTextInputEvent::Select);
            }

            // Moves the insertion pointer down in the current content.
            "moveDown:" => {
                driver.move_down();
                return Some(GeneratedTextInputEvent::Select);
            }
            // Moves the insertion pointer forward in the current content.
            "moveForward:" => {
                driver.move_right();
                return Some(GeneratedTextInputEvent::Select);
            } // TODO: Bidi-aware

            // Moves the insertion pointer left in the current content.
            "moveLeft:" => {
                driver.move_left();
                return Some(GeneratedTextInputEvent::Select);
            }
            // Moves the insertion pointer right in the current content.
            "moveRight:" => {
                driver.move_right();
                return Some(GeneratedTextInputEvent::Select);
            }
            // Moves the insertion pointer up in the current content.
            "moveUp:" => {
                driver.move_up();
                return Some(GeneratedTextInputEvent::Select);
            }

            // Modifying the Selection

            // Extends the selection to include the content before the current selection.
            "moveBackwardAndModifySelection:" => {
                driver.select_left(); // TODO: Bidi-aware
                return Some(GeneratedTextInputEvent::Select);
            }
            // Extends the selection to include the content below the current selection.
            "moveDownAndModifySelection:" => {
                driver.select_down();
                return Some(GeneratedTextInputEvent::Select);
            }
            // Extends the selection to include the content after the current selection.
            "moveForwardAndModifySelection:" => {
                driver.select_right(); // TODO: Bidi-aware
                return Some(GeneratedTextInputEvent::Select);
            }
            // Extends the selection to include the content to the left of the current selection.
            "moveLeftAndModifySelection:" => {
                driver.select_left();
                return Some(GeneratedTextInputEvent::Select);
            }
            // Extends the selection to include the content to the right of the current selection.
            "moveRightAndModifySelection:" => {
                driver.select_right();
                return Some(GeneratedTextInputEvent::Select);
            }
            // Extends the selection to include the content above the current selection.
            "moveUpAndModifySelection:" => {
                driver.select_up();
                return Some(GeneratedTextInputEvent::Select);
            }

            // Changing the Selection
            "selectAll:" => {
                driver.select_all();
                return Some(GeneratedTextInputEvent::Select);
            }
            "selectLine:" => {
                driver.move_to_line_start();
                driver.select_to_line_end();
                return Some(GeneratedTextInputEvent::Select);
            }
            "selectParagraph:" => {
                driver.move_to_hard_line_start();
                driver.select_to_hard_line_end();
                return Some(GeneratedTextInputEvent::Select);
            }
            "selectWord:" => {
                // TODO
            }

            // Moving the Selection in Documents
            "moveToBeginningOfDocument:" => {
                driver.move_to_text_start();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveToBeginningOfDocumentAndModifySelection:" => {
                driver.select_to_text_start();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveToEndOfDocument:" => {
                driver.move_to_text_end();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveToEndOfDocumentAndModifySelection:" => {
                driver.move_to_text_end();
                return Some(GeneratedTextInputEvent::Select);
            }

            // Moving the Selection in Paragraphs
            "moveParagraphBackwardAndModifySelection:" => {}
            "moveParagraphForwardAndModifySelection:" => {}
            "moveToBeginningOfParagraph:" => {
                driver.move_to_hard_line_start();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveToBeginningOfParagraphAndModifySelection:" => {
                driver.select_to_hard_line_start();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveToEndOfParagraph:" => {
                driver.move_to_hard_line_end();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveToEndOfParagraphAndModifySelection:" => {
                driver.select_to_hard_line_end();
                return Some(GeneratedTextInputEvent::Select);
            }

            // Moving the Selection in Lines of Text
            "moveToBeginningOfLine:" => {
                driver.move_to_line_start();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveToBeginningOfLineAndModifySelection:" => {
                driver.select_to_line_start();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveToEndOfLine:" => {
                driver.move_to_line_end();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveToEndOfLineAndModifySelection:" => {
                driver.select_to_line_end();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveToLeftEndOfLine:" => {
                driver.move_to_text_start();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveToLeftEndOfLineAndModifySelection:" => {
                driver.select_to_line_start();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveToRightEndOfLine:" => {
                driver.move_to_line_end();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveToRightEndOfLineAndModifySelection:" => {
                driver.select_to_line_end();
                return Some(GeneratedTextInputEvent::Select);
            }

            // Moving the Selection by Word Boundaries
            "moveWordBackward:" => {
                driver.move_word_left();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveWordBackwardAndModifySelection:" => {
                driver.select_word_left();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveWordForward:" => {
                driver.move_word_right();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveWordForwardAndModifySelection:" => {
                driver.select_word_right();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveWordLeft:" => {
                driver.move_word_left();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveWordLeftAndModifySelection:" => {
                driver.select_word_left();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveWordRight:" => {
                driver.move_word_right();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveWordRightAndModifySelection:" => {
                driver.select_word_right();
                return Some(GeneratedTextInputEvent::Select);
            }

            // Scrolling Content

            // Scrolls the content down by a page.
            "scrollPageDown:" => {}
            // Scrolls the content up by a page.
            "scrollPageUp:" => {}
            // Scrolls the content down by a line.
            "scrollLineDown:" => {}
            // Scrolls the content up by a line.
            "scrollLineUp:" => {}
            // Scrolls the content to the beginning of the document.
            "scrollToBeginningOfDocument:" => {}
            // Scrolls the content to the end of the document.
            "scrollToEndOfDocument:" => {}
            // Moves the visible content region down by a page.
            "pageDown:" => {}
            // Moves the visible content region up by a page.
            "pageUp:" => {}
            // Moves the visible content region down by a page, and extends the current selection.
            "pageDownAndModifySelection:" => {}
            // Moves the visible content region up by a page, and extends the current selection.
            "pageUpAndModifySelection:" => {}
            // Moves the visible content region so the current selection is visually centered.
            "centerSelectionInVisibleArea:" => {}

            // Transposing Elements

            // Transposes the content around the current selection.
            "transpose:" => {}
            // Transposes the words around the current selection.
            "transposeWords:" => {}

            // Indenting Content
            // Indents the content at the current selection.
            "indent:" => {}

            // Canceling Operations
            // Cancels the current operation.
            "cancelOperation:" => {}

            // Supporting QuickLook
            // Invokes QuickLook to preview the current selection.
            "quickLookPreviewItems:" => {}

            // Supporting Writing Directions
            "makeBaseWritingDirectionLeftToRight:" => {}
            "makeBaseWritingDirectionNatural:" => {}
            "makeBaseWritingDirectionRightToLeft:" => {}
            "makeTextWritingDirectionLeftToRight:" => {}
            "makeTextWritingDirectionNatural:" => {}
            "makeTextWritingDirectionRightToLeft:" => {}

            // Changing Capitalization
            "capitalizeWord:" => {}
            "changeCaseOfLetter:" => {}
            "lowercaseWord:" => {}
            "uppercaseWord:" => {}

            // Supporting Marked Selections
            "setMark:" => {}
            "selectToMark:" => {}
            "deleteToMark:" => {}
            "swapWithMark:" => {}

            // Supporting Autocomplete
            "complete:" => {}

            // Instance Methods
            "showContextMenuForSelection:" => {}

            // Unknown command
            _ => {}
        };

        None
    }

    pub(crate) fn apply_ime_event(
        &mut self,
        font_ctx: &mut FontContext,
        layout_ctx: &mut LayoutContext<TextBrush>,
        event: BlitzImeEvent,
    ) -> Option<GeneratedTextInputEvent> {
        let editor = &mut self.editor;
        let mut driver = editor.driver(font_ctx, layout_ctx);

        match event {
            BlitzImeEvent::Enabled => {
                // Do nothing
                None
            }
            BlitzImeEvent::Disabled => {
                driver.clear_compose();
                Some(GeneratedTextInputEvent::PreEditChange)
            }
            BlitzImeEvent::Commit(text) => {
                driver.insert_or_replace_selection(&text);
                Some(GeneratedTextInputEvent::Input)
            }
            BlitzImeEvent::Preedit(text, cursor) => {
                if text.is_empty() {
                    driver.clear_compose();
                } else {
                    driver.set_compose(&text, cursor);
                }
                Some(GeneratedTextInputEvent::PreEditChange)
            }
            BlitzImeEvent::DeleteSurrounding {
                before_bytes,
                after_bytes,
            } => {
                let _ = before_bytes;
                let _ = after_bytes;
                // TODO
                None
            }
        }
    }
}

#[cfg(test)]
mod content_widths_cache_tests {
    use super::*;
    use parley::{InlineBox, InlineBoxKind, TextStyle};

    /// Build a [`TextLayout`] containing `text`, optionally followed by an inline box of
    /// `inline_box_width` pixels.
    fn build_layout(text: &str, inline_box_width: Option<f32>) -> TextLayout {
        let mut font_ctx = FontContext::default();
        let mut layout_ctx = LayoutContext::new();
        let style: TextStyle<'_, '_, TextBrush> = TextStyle::default();
        let mut builder = layout_ctx.tree_builder(&mut font_ctx, 1.0, true, &style);
        builder.push_text(text);
        if let Some(width) = inline_box_width {
            builder.push_inline_box(InlineBox {
                id: 0,
                kind: InlineBoxKind::InFlow,
                index: text.len(),
                width,
                height: 10.0,
            });
        }

        let mut text_layout = TextLayout::new();
        text_layout.text = builder.build_into(&mut text_layout.layout);
        text_layout
    }

    #[test]
    fn first_call_matches_an_uncached_computation() {
        let mut text_layout = build_layout("the quick brown fox", None);
        let expected = text_layout.layout.calculate_content_widths();

        let cached = text_layout.content_widths();

        assert_eq!(cached.min, expected.min);
        assert_eq!(cached.max, expected.max);
        assert!(cached.min > 0.0);
        assert!(cached.max > cached.min);
    }

    #[test]
    fn text_only_layout_reuses_the_cached_widths() {
        let mut text_layout = build_layout("the quick brown fox", None);
        text_layout.content_widths();

        // Poison the stored result. A second call that recomputed would overwrite this with
        // the real widths, so seeing the poisoned value back proves the cache was hit.
        let poison = ContentWidths {
            min: -1.0,
            max: -2.0,
        };
        text_layout.content_widths.as_mut().unwrap().widths = poison;

        let second = text_layout.content_widths();
        assert_eq!(second.min, poison.min);
        assert_eq!(second.max, poison.max);
    }

    #[test]
    fn a_changed_inline_box_width_forces_a_recompute() {
        let mut text_layout = build_layout("the quick brown fox", Some(40.0));
        let first = text_layout.content_widths();

        // Same poison as above, so a stale hit would be visible.
        text_layout.content_widths.as_mut().unwrap().widths = ContentWidths {
            min: -1.0,
            max: -2.0,
        };

        // Re-measuring the inline box under a different constraint is exactly what block
        // layout does between a min-content and a max-content pass.
        text_layout.layout.inline_boxes_mut()[0].width = 400.0;

        let second = text_layout.content_widths();
        assert!(second.min > 0.0);
        assert!(second.max > first.max);
        assert_eq!(second.min, 400.0);
    }

    #[test]
    fn an_unchanged_inline_box_width_still_hits_the_cache() {
        let mut text_layout = build_layout("the quick brown fox", Some(40.0));
        text_layout.content_widths();

        let poison = ContentWidths {
            min: -1.0,
            max: -2.0,
        };
        text_layout.content_widths.as_mut().unwrap().widths = poison;
        // Write the identical width back; the key is unchanged so this must not recompute.
        text_layout.layout.inline_boxes_mut()[0].width = 40.0;

        let second = text_layout.content_widths();
        assert_eq!(second.min, poison.min);
        assert_eq!(second.max, poison.max);
    }

    #[test]
    fn rebuilding_the_layout_discards_the_cache() {
        let mut text_layout = build_layout("the quick brown fox", None);
        text_layout.content_widths();
        assert!(text_layout.content_widths.is_some());

        // Stand in for `build_inline_layout_into`, which clears the cache before re-shaping.
        text_layout.content_widths = None;
        let rebuilt = build_layout("a much much much longer run of text", None);
        text_layout.layout = rebuilt.layout;
        text_layout.text = rebuilt.text;

        let widths = text_layout.content_widths();
        let expected = text_layout.layout.calculate_content_widths();
        assert_eq!(widths.max, expected.max);
    }
}

#[cfg(test)]
mod shortcut_tests {
    use super::*;
    use blitz_traits::events::{BlitzKeyEvent, KeyState};
    use blitz_traits::shell::DummyShellProvider;
    use keyboard_types::Location;

    fn control_event(key: Key, code: Code) -> BlitzKeyEvent {
        BlitzKeyEvent {
            key,
            code,
            modifiers: Modifiers::CONTROL,
            location: Location::Standard,
            is_auto_repeating: false,
            is_composing: false,
            state: KeyState::Pressed,
            text: None,
        }
    }

    #[test]
    fn control_character_cut_uses_the_physical_key_code() {
        let event = control_event(Key::Character("\u{18}".into()), Code::KeyX);
        assert_eq!(clipboard_command(&event), Some(ClipboardCommand::Cut));
    }

    #[test]
    fn backspace_does_not_depend_on_an_apple_standard_keybinding() {
        let mut data = TextInputData::new(false);
        let mut font_ctx = FontContext::default();
        let mut layout_ctx = LayoutContext::new();
        data.set_text(&mut font_ctx, &mut layout_ctx, "typo");
        data.editor
            .driver(&mut font_ctx, &mut layout_ctx)
            .move_to_text_end();
        let event = BlitzKeyEvent {
            key: Key::Backspace,
            code: Code::Backspace,
            modifiers: Modifiers::empty(),
            location: Location::Standard,
            is_auto_repeating: false,
            is_composing: false,
            state: KeyState::Pressed,
            text: None,
        };

        assert!(matches!(
            data.apply_keypress_event(&mut font_ctx, &mut layout_ctx, &DummyShellProvider, event,),
            Some(GeneratedTextInputEvent::Input)
        ));
        assert_eq!(data.editor.raw_text(), "typ");
    }
}
