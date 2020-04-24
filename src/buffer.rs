use core::ops::RangeBounds;
use std::{
    cell::RefCell,
    cmp::{ min, max },
    fs::File,
    ffi::OsStr,
    iter::once,
    os::windows::ffi::OsStrExt,
    ptr::null_mut,
    mem::{ swap, MaybeUninit },
    rc::Rc,
    char,
    str
};
use winapi::{
    um::{
        dwrite::{ IDWriteTextLayout, DWRITE_HIT_TEST_METRICS, DWRITE_TEXT_RANGE },
        d2d1::{ D2D1_RECT_F, D2D1_LAYER_PARAMETERS },
        winuser::{ SystemParametersInfoW, SPI_GETCARETWIDTH }
    },
    ctypes::c_void
};
use ropey::Rope;

use crate::dx_ok;
use crate::settings;
use crate::renderer::TextRenderer;

#[derive(PartialEq)]
pub enum SelectionMode {
    Left,
    Right,
    Down,
    Up
}

#[derive(PartialEq)]
pub enum MouseSelectionMode {
    Click,
    Move
}

pub struct TextBuffer {
    buffer: Rope,

    // The layout of the text buffer should be public for
    // the renderer to use
    pub origin: (u32, u32),
    pub extents: (u32, u32),
    pub text_origin: (u32, u32),
    pub text_extents: (u32, u32),
    pub text_visible_line_count: usize,
    pub line_numbers_origin: (u32, u32),
    pub line_numbers_extents: (u32, u32),
    pub line_numbers_margin: u32,

    // The selection state of the buffer should be public
    // for the editor to use
    pub currently_selecting: bool,

    top_line: usize,
    bot_line: usize,
    absolute_char_pos_start: usize,
    absolute_char_pos_end: usize,

    caret_char_anchor: usize,
    caret_char_pos: usize,
    caret_is_trailing: i32,
    caret_width: u32,
    half_caret_width: u32,

    cached_char_offset: u32,

    text_layer_params: D2D1_LAYER_PARAMETERS,
    text_layout: *mut IDWriteTextLayout,

    line_numbers_layer_params: D2D1_LAYER_PARAMETERS,
    line_numbers_layout: *mut IDWriteTextLayout,

    renderer: Rc<RefCell<TextRenderer>>
}

impl TextBuffer {
    pub fn new(path: &str, origin: (u32, u32), extents: (u32, u32), renderer: Rc<RefCell<TextRenderer>>) -> TextBuffer {
        let file = File::open(path).unwrap();
        let buffer = Rope::from_reader(file).unwrap();

        let mut caret_width: u32 = 0;
        unsafe {
            // We'll increase the width from the system width slightly
            SystemParametersInfoW(SPI_GETCARETWIDTH, 0, (&mut caret_width as *mut _) as *mut c_void, 0);
            caret_width *= 3;
        }

        let mut text_buffer = TextBuffer {
            buffer,

            origin,
            extents,
            text_origin: (0, 0),
            text_extents: (0, 0),
            text_visible_line_count: 0,
            line_numbers_origin: (0, 0),
            line_numbers_extents: (0, 0),
            line_numbers_margin: 0,

            currently_selecting: false,

            top_line: 0,
            bot_line: 0,
            absolute_char_pos_start: 0,
            absolute_char_pos_end: 0,

            caret_char_anchor: 0,
            caret_char_pos: 0,
            caret_is_trailing: 0,
            caret_width,
            half_caret_width: caret_width / 2,

            cached_char_offset: 0,

            text_layer_params: unsafe { MaybeUninit::<D2D1_LAYER_PARAMETERS>::zeroed().assume_init() },
            text_layout: null_mut(),

            line_numbers_layer_params: unsafe { MaybeUninit::<D2D1_LAYER_PARAMETERS>::zeroed().assume_init() },
            line_numbers_layout: null_mut(),

            renderer
        };

        text_buffer.update_metrics(origin, extents);
        text_buffer
    }

    pub fn get_caret_absolute_pos(&self) -> usize {
        self.caret_char_pos + (self.caret_is_trailing as usize)
    }

    pub fn scroll_down(&mut self) {
        let new_top = self.top_line + settings::MOUSEWHEEL_LINES_PER_ROLL;
        if new_top >= self.buffer.len_lines() {
            self.top_line = self.buffer.len_lines() - 1;
        }
        else {
            self.top_line = new_top;
        }
        self.update_absolute_char_positions();
    }

    pub fn scroll_up(&mut self) {
        if self.top_line >= settings::MOUSEWHEEL_LINES_PER_ROLL {
            self.top_line -= settings::MOUSEWHEEL_LINES_PER_ROLL;
        }
        else {
            self.top_line = 0;
        }
        self.update_absolute_char_positions();
    } 

    pub fn move_left(&mut self, shift_down: bool) {
        let mut count = 1;
        if self.see_prev_chars("\r\n") {
            count = 2;
        }
        self.set_selection(SelectionMode::Left, count, shift_down);
    }

    pub fn move_right(&mut self, shift_down: bool) {
        let mut count = 1;
        if self.see_chars("\r\n") {
            count = 2;
        }
        self.set_selection(SelectionMode::Right, count, shift_down);
    }

    pub fn left_click(&mut self, mouse_pos: (f32, f32), extend_current_selection: bool) {
        self.set_mouse_selection(MouseSelectionMode::Click, mouse_pos);
        if !extend_current_selection {
            self.caret_char_anchor = self.get_caret_absolute_pos();
        }
        self.currently_selecting = true;

        // Reset the cached width
        self.cached_char_offset = 0;
    }

    pub fn left_release(&mut self) {
        self.currently_selecting = false;
    }

    fn linebreaks_before_line(&self, line: usize) -> usize {
        let mut line_start = self.buffer.chars_at(self.buffer.line_to_char(line));
        match line_start.prev() {
            Some('\n') => {
                if line_start.prev() == Some('\r') {
                    return 2;
                }
                else {
                    return 1;
                }
            },

            // For completeness, we will count all linebreaks
            // that ropey supports
            Some('\u{000B}') => 1,
            Some('\u{000C}') => 1,
            Some('\u{000D}') => 1,
            Some('\u{0085}') => 1,
            Some('\u{2028}') => 1,
            Some('\u{2029}') => 1,
            _ => 0
        }
    }

    pub fn set_selection(&mut self, mode: SelectionMode, count: usize, extend_current_selection: bool) {
        let caret_absolute_pos = self.get_caret_absolute_pos();

        match mode {
            SelectionMode::Left | SelectionMode::Right => {
                self.caret_char_pos = caret_absolute_pos;

                if mode == SelectionMode::Left {
                    if self.caret_char_pos > 0 {
                        self.caret_char_pos -= count;
                    }
                }
                else {
                    if self.caret_char_pos < self.buffer.len_chars() {
                        self.caret_char_pos += count;
                    }
                }
                self.caret_is_trailing = 0;

                // Reset the cached width
                self.cached_char_offset = 0;
            },
            SelectionMode::Up | SelectionMode::Down => {
                let current_line = self.buffer.char_to_line(caret_absolute_pos);

                let target_line_idx;
                let target_linebreak_count;
                if mode == SelectionMode::Up {
                    // If we're on the first line, return
                    if current_line == 0 {
                        return;
                    }
                    target_line_idx = current_line - 1;
                    target_linebreak_count = self.linebreaks_before_line(current_line);
                }
                else {
                    // If we're on the last line, return
                    if current_line == self.buffer.len_lines() - 1 {
                        return;
                    }
                    target_line_idx = current_line + 1;
                    target_linebreak_count = self.linebreaks_before_line(target_line_idx);
                }

                let target_line = self.buffer.line(target_line_idx);
                let target_line_length = target_line.len_chars().saturating_sub(target_linebreak_count);

                let current_offset = caret_absolute_pos - self.buffer.line_to_char(current_line);
                let desired_offset = max(self.cached_char_offset, current_offset as u32);
                self.cached_char_offset = desired_offset;

                let new_offset = min(target_line_length, desired_offset as usize);

                self.caret_char_pos = self.buffer.line_to_char(target_line_idx) + new_offset;
                self.caret_is_trailing = 0;
            },
        }

        if !extend_current_selection {
            self.caret_char_anchor = self.get_caret_absolute_pos();
        }

    }

    pub fn set_mouse_selection(&mut self, mode: MouseSelectionMode, mouse_pos: (f32, f32)) {
        let relative_mouse_pos = self.translate_mouse_pos_to_text_region(mouse_pos);
        if mode == MouseSelectionMode::Click || (mode == MouseSelectionMode::Move && self.currently_selecting) {
            let mut is_inside = 0;
            let mut metrics_uninit = MaybeUninit::<DWRITE_HIT_TEST_METRICS>::uninit();

            unsafe {
                dx_ok!(
                    (*self.text_layout).HitTestPoint(
                        relative_mouse_pos.0,
                        relative_mouse_pos.1,
                        &mut self.caret_is_trailing,
                        &mut is_inside,
                        metrics_uninit.as_mut_ptr()
                    )
                );

                let metrics = metrics_uninit.assume_init();
                let absolute_text_pos = metrics.textPosition as usize;

                self.caret_char_pos = self.absolute_char_pos_start + absolute_text_pos;
            }

            // If we're at the end of the rope, the caret may not be trailing
            // otherwise we will be inserting out of bounds on the rope
            if self.caret_char_pos == self.buffer.len_chars() {
                self.caret_is_trailing = 0;
            }
        }
    }

    fn translate_mouse_pos_to_text_region(&self, mouse_pos: (f32, f32)) -> (f32, f32) {
        let dx = mouse_pos.0 - self.text_origin.0 as f32;
        let dy = mouse_pos.1 - self.text_origin.1 as f32;
        (dx, dy)
    }

    pub fn delete_selection(&mut self) {
        let caret_absolute_pos = self.get_caret_absolute_pos();
        if caret_absolute_pos != self.caret_char_anchor {
            if caret_absolute_pos < self.caret_char_anchor {
                self.buffer.remove(caret_absolute_pos..self.caret_char_anchor);
                self.caret_char_pos = caret_absolute_pos;
                self.caret_char_anchor = self.caret_char_pos;
            }
            else {
                self.buffer.remove(self.caret_char_anchor..caret_absolute_pos);
                let caret_anchor_delta = caret_absolute_pos - self.caret_char_anchor;
                self.caret_char_pos = caret_absolute_pos - caret_anchor_delta;
            }
            self.caret_is_trailing = 0;
        }
    }

    pub fn clear_selection(&mut self) {
        self.caret_char_anchor = self.caret_char_pos;
    }

    pub fn insert_chars(&mut self, chars: &str) {
        self.delete_selection();

        self.buffer.insert(self.get_caret_absolute_pos(), chars);
        self.set_selection(SelectionMode::Right, chars.len(), false);

        self.update_absolute_char_positions();
    }

    pub fn insert_char(&mut self, character: u16) {
        self.delete_selection();

        self.buffer.insert_char(self.get_caret_absolute_pos(), (character as u8) as char);
        self.set_selection(SelectionMode::Right, 1, false);

        self.update_absolute_char_positions();
    }

    fn see_chars(&mut self, string: &str) -> bool {
        let mut rope_iterator = self.buffer.chars_at(self.get_caret_absolute_pos());
        for chr in string.chars() {
            match rope_iterator.next() {
                Some(x) if x == chr => continue,
                _ => return false,
            }
        }
        true
    }

    fn see_prev_chars(&mut self, string: &str) -> bool {
        let mut rope_iterator = self.buffer.chars_at(self.get_caret_absolute_pos());
        for chr in string.chars().rev() {
            match rope_iterator.prev() {
                Some(x) if x == chr => continue,
                _ => return false,
            }
        }
        true
    }

    pub fn delete_char(&mut self) {
        let caret_absolute_pos = min(self.get_caret_absolute_pos(), self.buffer.len_chars());

        // If we are currently selecting text, 
        // simply delete the selected text
        if caret_absolute_pos != self.caret_char_anchor {
            self.delete_selection();
            return;
        }

        // In case of a CRLF, delete both characters
        let mut offset = 1;
        if self.see_chars("\r\n") {
            offset = 2;
        }

        let next_char_pos = min(caret_absolute_pos + offset, self.buffer.len_chars());
        self.buffer.remove(caret_absolute_pos..next_char_pos);

        self.clear_selection();
        self.update_absolute_char_positions();
    }

    pub fn delete_previous_char(&mut self) {
        let caret_absolute_pos = min(self.get_caret_absolute_pos(), self.buffer.len_chars());

        // If we are currently selecting text, 
        // simply delete the selected text
        if caret_absolute_pos != self.caret_char_anchor {
            self.delete_selection();
            return;
        }

        // In case of a CRLF, delete both characters
        let mut offset = 1;
        if self.see_prev_chars("\r\n") {
            offset = 2;
        }

        let previous_char_pos = caret_absolute_pos.saturating_sub(offset);
        self.buffer.remove(previous_char_pos..caret_absolute_pos);
        self.set_selection(SelectionMode::Left, offset, false);

        self.clear_selection();
        self.update_absolute_char_positions();
    }

    pub fn get_caret_rect(&mut self) -> Option<D2D1_RECT_F> {
        if self.caret_char_pos < self.absolute_char_pos_start {
            return None;
        }

        let mut caret_pos: (f32, f32) = (0.0, 0.0);
        let mut metrics_uninit = MaybeUninit::<DWRITE_HIT_TEST_METRICS>::uninit();

        unsafe {
            dx_ok!((*self.text_layout).HitTestTextPosition(
                (self.caret_char_pos - self.absolute_char_pos_start) as u32,
                self.caret_is_trailing,
                &mut caret_pos.0,
                &mut caret_pos.1,
                metrics_uninit.as_mut_ptr()
            ));

            let metrics = metrics_uninit.assume_init();

            let rect = D2D1_RECT_F {
                left: self.text_origin.0 as f32 + caret_pos.0 - self.half_caret_width as f32,
                top: self.text_origin.1 as f32 + caret_pos.1,
                right: self.text_origin.0 as f32 + caret_pos.0 + (self.caret_width - self.half_caret_width) as f32,
                bottom: self.text_origin.1 as f32 + caret_pos.1 + metrics.height
            };

            return Some(rect)
        }
    }

    pub fn get_selection_range(&self) -> Option<DWRITE_TEXT_RANGE> {
        let caret_absolute_pos = self.get_caret_absolute_pos();
        if caret_absolute_pos == self.caret_char_anchor {
            return None;
        }
 
        // Saturating sub ensures that the carets don't go below 0
        let mut caret_begin = self.caret_char_anchor.saturating_sub(self.absolute_char_pos_start);
        let mut caret_end = caret_absolute_pos.saturating_sub(self.absolute_char_pos_start);

        if caret_begin > caret_end {
            swap(&mut caret_begin, &mut caret_end);
        }

        caret_begin = min(caret_begin, self.absolute_char_pos_end);
        caret_end = min(caret_end, self.absolute_char_pos_end);

        let range =  DWRITE_TEXT_RANGE {
            startPosition: caret_begin as u32,
            length: (caret_end - caret_begin) as u32
        };

        Some(range)
    }

    pub fn get_text_layout(&mut self) -> (*mut IDWriteTextLayout, D2D1_LAYER_PARAMETERS) {
        let lines = self.get_current_lines();

        unsafe {
            if !self.text_layout.is_null() {
                (*self.text_layout).Release();
            }

            dx_ok!((*self.renderer.borrow().write_factory).CreateTextLayout(
                lines.as_ptr(),
                lines.len() as u32,
                self.renderer.borrow().text_format,
                self.text_extents.0 as f32,
                self.text_extents.1 as f32,
                &mut self.text_layout as *mut *mut _
            ));
        }

        (self.text_layout, self.text_layer_params)
    }

    pub fn get_line_numbers_layout(&mut self) -> (*mut IDWriteTextLayout, D2D1_LAYER_PARAMETERS) {
        let mut nums: String = String::new();
        let number_range_end = min(self.buffer.len_lines() - 1, self.bot_line);

        for i in self.top_line..=number_range_end {
            nums += (i + 1).to_string().as_str();
            nums += "\r\n";
        }
        let lines: Vec<u16> = OsStr::new(nums.as_str()).encode_wide().chain(once(0)).collect();

        unsafe {
            if !self.line_numbers_layout.is_null() {
                (*self.line_numbers_layout).Release();
            }

            dx_ok!((*self.renderer.borrow().write_factory).CreateTextLayout(
                lines.as_ptr(),
                lines.len() as u32,
                self.renderer.borrow().text_format,
                self.line_numbers_extents.0 as f32,
                self.line_numbers_extents.1 as f32,
                &mut self.line_numbers_layout as *mut *mut _
            ));
        }

        (self.line_numbers_layout, self.line_numbers_layer_params)
    }

    pub fn update_metrics(&mut self, origin: (u32, u32), extents: (u32, u32)) {
        self.origin = origin;
        self.extents = extents;

        self.update_line_numbers_margin();
        self.update_text_region();
        self.update_numbers_region();
        self.update_text_visible_line_count();
        self.update_absolute_char_positions();
    }

    fn update_line_numbers_margin(&mut self) {
        let end_line_max_digits = self.get_digits_in_number(self.buffer.len_lines() as u32);
        let font_width = self.renderer.borrow().font_width;
        self.line_numbers_margin = (end_line_max_digits * font_width as u32) + (font_width / 2.0) as u32;
    }

    fn update_text_region(&mut self) {
        self.text_origin = (
            self.origin.0 + self.line_numbers_margin,
            self.origin.1
        );
        self.text_extents = (
            self.extents.0 - self.line_numbers_margin,
            self.extents.1
        );
        self.text_layer_params = TextRenderer::layer_params(self.text_origin, self.text_extents);
    }

    fn update_numbers_region(&mut self) {
        self.line_numbers_origin = (self.origin.0, self.origin.1);
        self.line_numbers_extents = (
            self.line_numbers_margin,
            self.extents.1
        );
        self.line_numbers_layer_params = TextRenderer::layer_params(
            self.line_numbers_origin, 
            self.line_numbers_extents
        );
    }

    fn update_text_visible_line_count(&mut self) {
        let max_lines_in_text_region = self.extents.1 as usize / self.renderer.borrow().font_height as usize;
        self.text_visible_line_count = min(self.buffer.len_lines(), max_lines_in_text_region);
    }

    fn update_absolute_char_positions(&mut self) {
        self.bot_line = self.top_line + (self.text_visible_line_count - 1);
        self.absolute_char_pos_start = self.buffer.line_to_char(self.top_line);
        if self.bot_line >= self.buffer.len_lines() {
            self.absolute_char_pos_end = self.buffer.line_to_char(self.buffer.len_lines());
        }
        else {
            self.absolute_char_pos_end = self.buffer.line_to_char(self.bot_line);
        }
    }

    pub fn get_current_lines(&self) -> Vec<u16> {
        self.text_range(self.absolute_char_pos_start..self.absolute_char_pos_end)
    }

    fn text_range<R>(&self, char_range: R) -> Vec<u16> where R: RangeBounds<usize> {
        let rope_slice = self.buffer.slice(char_range);
        let chars: Vec<u8> = rope_slice.bytes().collect();
        OsStr::new(str::from_utf8(chars.as_ref()).unwrap()).encode_wide().chain(once(0)).collect()
    }

    fn get_digits_in_number(&self, number: u32) -> u32 {
        match number {
            0..=9 => 1,
            10..=99 => 2,
            100..=999 => 3,
            1000..=9999 => 4,
            10000..=99999 => 5,
            100000..=999999 => 6,
            1000000..=9999999 => 7,
            10000000..=99999999 => 8,
            100000000..=999999999 => 9,
            1000000000..=4294967295 => 10
        }
    }
}
