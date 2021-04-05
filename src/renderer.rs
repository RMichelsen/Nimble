use crate::{
    settings,
    buffer::TextBuffer,
    theme::Theme,
    language_support::SemanticTokenTypes,
    util::pwstr_from_str
};

use std::{
    collections::HashMap,
    ptr::null_mut
};

use bindings::{
    Windows::Win32::WindowsAndMessaging::*,
    Windows::Win32::HiDpi::*,
    Windows::Win32::Dxgi::*,
    Windows::Win32::DirectWrite::*,
    Windows::Win32::Direct2D::*,
    Windows::Win32::DisplayDevices::*,
    Windows::Win32::SystemServices::*,
    Windows::Foundation::Numerics::*
};
use windows::{Abi, Result, Interface};

fn get_client_size(hwnd: HWND) -> D2D_SIZE_U {
    let mut rect = RECT::default();
    unsafe { GetClientRect(hwnd, &mut rect); }
    D2D_SIZE_U {
        width: (rect.right - rect.left) as u32,
        height: (rect.bottom - rect.top) as u32
    }
}

fn create_dwrite_factory() -> Result<IDWriteFactory> {
    let mut write_factory = None;

    unsafe {
        DWriteCreateFactory(
            DWRITE_FACTORY_TYPE::DWRITE_FACTORY_TYPE_SHARED, 
            &IDWriteFactory::IID, 
            write_factory.set_abi() as _
        ).and_some(write_factory)
    }
}

fn create_text_format(font_name: PWSTR, font_locale: PWSTR, font_size: f32, dwrite_factory: &IDWriteFactory) -> Result<IDWriteTextFormat> {
    unsafe {
        let mut text_format = None;
        dwrite_factory.CreateTextFormat(
            font_name,
            None,
            DWRITE_FONT_WEIGHT::DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_FONT_STYLE::DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH::DWRITE_FONT_STRETCH_NORMAL,
            font_size,
            font_locale,
            &mut text_format
        ).and_some(text_format)
    }
}

fn create_d2d1_factory() -> Result<ID2D1Factory> {
    let mut d2d1_factory = None;
    unsafe {
        D2D1CreateFactory(
            D2D1_FACTORY_TYPE::D2D1_FACTORY_TYPE_SINGLE_THREADED, 
            &ID2D1Factory::IID,
            null_mut(), 
            d2d1_factory.set_abi()
        ).and_some(d2d1_factory)
    }
}

fn create_render_target(d2d1_factory: &ID2D1Factory, hwnd: HWND) -> Result<ID2D1HwndRenderTarget> {
    let target_props = D2D1_RENDER_TARGET_PROPERTIES {
        r#type: D2D1_RENDER_TARGET_TYPE::D2D1_RENDER_TARGET_TYPE_DEFAULT,
        pixelFormat: D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT::DXGI_FORMAT_UNKNOWN,
            alphaMode: D2D1_ALPHA_MODE::D2D1_ALPHA_MODE_UNKNOWN
        },
        dpiX: 96.0,
        dpiY: 96.0,
        usage: D2D1_RENDER_TARGET_USAGE::D2D1_RENDER_TARGET_USAGE_NONE,
        minLevel: D2D1_FEATURE_LEVEL::D2D1_FEATURE_LEVEL_DEFAULT
    };

    let hwnd_props = D2D1_HWND_RENDER_TARGET_PROPERTIES {
        hwnd,
        pixelSize: get_client_size(hwnd),
        presentOptions: D2D1_PRESENT_OPTIONS::D2D1_PRESENT_OPTIONS_NONE
    };

    let mut render_target = None;
    unsafe {
        d2d1_factory.CreateHwndRenderTarget(&target_props, &hwnd_props, &mut render_target).and_some(render_target)
    }
}

fn get_font_width_and_height(dwrite_factory: &IDWriteFactory, text_format: &IDWriteTextFormat) -> Result<(f32, f32)> {
    unsafe {
        let mut temp_text_layout = None;
        let text_layout = dwrite_factory.CreateTextLayout(
            pwstr_from_str("a"),
            1,
            text_format,
            0.0,
            0.0,
            &mut temp_text_layout
        ).and_some(temp_text_layout)?;
        
        let mut metrics = DWRITE_HIT_TEST_METRICS::default();
        let mut dummy: (f32, f32) = (0.0, 0.0);
        text_layout.HitTestTextPosition(
            0,
            false,
            &mut dummy.0,
            &mut dummy.1,
            &mut metrics
        ).ok()?;

        Ok((metrics.width, metrics.height))
    }
}

pub struct TextLayout {
    origin: (f32, f32),
    extents: (f32, f32),
    layout: IDWriteTextLayout
}

pub struct TextRenderer {
    pub pixel_size: D2D_SIZE_U,
    pub font_size: f32,
    pub font_height: f32,
    pub font_width: f32,
    font_name: String,

    caret_width: u32,

    theme: Theme,

    dwrite_factory: IDWriteFactory,
    text_format: IDWriteTextFormat,
    
    render_target: ID2D1HwndRenderTarget,

    buffer_layouts: HashMap<String, TextLayout>,
    buffer_line_number_layouts: HashMap<String, TextLayout>
}

impl TextRenderer {
    pub fn new(hwnd: HWND, font: &str, font_size: f32) -> Result<Self> {
        unsafe {
            // We'll increase the width from the system width slightly
            let mut caret_width: u32 = 0;
            SystemParametersInfoW(SYSTEM_PARAMETERS_INFO_ACTION::SPI_GETCARETWIDTH, 0, (&mut caret_width as *mut _) as _, SystemParametersInfo_fWinIni(0));
            caret_width *= 2;

            let dpi = GetDpiForWindow(hwnd);
            let dpi_scale = dpi as f32 / 96.0;

            // Scale the font size to fit the dpi
            let scaled_font_size = font_size * dpi_scale;

            let dwrite_factory = create_dwrite_factory()?;

            let text_format = create_text_format(
                pwstr_from_str(font),
                pwstr_from_str("en-us"),
                scaled_font_size,
                &dwrite_factory
            )?;
            text_format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT::DWRITE_TEXT_ALIGNMENT_LEADING).ok()?;
            text_format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT::DWRITE_PARAGRAPH_ALIGNMENT_NEAR).ok()?;
            text_format.SetWordWrapping(DWRITE_WORD_WRAPPING::DWRITE_WORD_WRAPPING_NO_WRAP).ok()?;

            let (font_width, font_height) = get_font_width_and_height(&dwrite_factory, &text_format)?;
            text_format.SetIncrementalTabStop(font_width * settings::NUMBER_OF_SPACES_PER_TAB as f32).ok()?;

            let d2d1_factory = create_d2d1_factory()?;
            let render_target = create_render_target(&d2d1_factory, hwnd)?;

            Ok(Self {
                pixel_size: get_client_size(hwnd),
                font_size: scaled_font_size,
                font_height,
                font_width,
                font_name: String::from(font),
                caret_width,
                theme: Theme::new_default(&render_target)?,
                dwrite_factory,
                text_format,
                render_target,
                buffer_layouts: HashMap::new(),
                buffer_line_number_layouts: HashMap::new()
            })
        }
    }

    pub fn update_text_format(&mut self) -> Result<()> {
        unsafe {
            self.text_format = create_text_format(
                pwstr_from_str(&self.font_name),
                pwstr_from_str("en-us"),
                self.font_size,
                &self.dwrite_factory
            )?;
    
            self.text_format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT::DWRITE_TEXT_ALIGNMENT_LEADING).ok()?;
            self.text_format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT::DWRITE_PARAGRAPH_ALIGNMENT_NEAR).ok()?;
            self.text_format.SetWordWrapping(DWRITE_WORD_WRAPPING::DWRITE_WORD_WRAPPING_NO_WRAP).ok()?;
    
            let (font_width, font_height) = get_font_width_and_height(&self.dwrite_factory, &self.text_format)?;
            self.text_format.SetIncrementalTabStop(font_width * settings::NUMBER_OF_SPACES_PER_TAB as f32).ok()?;
            self.font_width = font_width;
            self.font_height = font_height;
        }

        Ok(())
    }

    pub fn get_max_rows(&self) -> usize {
        (self.pixel_size.height as f32 / self.font_height).ceil() as usize
    }

    pub fn get_max_columns(&self) -> usize {
        (self.pixel_size.width as f32 / self.font_width) as usize
    }

    pub fn get_extents(&self) -> (f32, f32) {
        (self.pixel_size.width as f32, self.pixel_size.height as f32)
    }

    fn get_text_buffer_margin(&self, text_buffer: &mut TextBuffer) -> f32 {
        text_buffer.margin_column_count as f32 * self.font_width
    }

    fn get_text_buffer_column_offset(&self, text_buffer: &mut TextBuffer) -> f32 {
        text_buffer.column_offset as f32 * self.font_width
    }

    fn get_text_buffer_adjusted_origin(&self, text_buffer: &mut TextBuffer) -> (f32, f32) {
        let margin = self.get_text_buffer_margin(text_buffer);
        let column_offset = self.get_text_buffer_column_offset(text_buffer);
        let text_layout = self.buffer_layouts.get(&text_buffer.path).unwrap();
        (text_layout.origin.0 + margin - column_offset, text_layout.origin.1)
    }

    pub fn update_buffer_layout(&mut self, origin: (f32, f32), extents: (f32, f32), text_buffer: &mut TextBuffer) -> Result<()> {
        let mut lines = text_buffer.get_text_view_as_utf16();
        let margin = self.get_text_buffer_margin(text_buffer);

        unsafe {
            let mut text_layout = None;
            self.dwrite_factory.CreateTextLayout(
                PWSTR(lines.as_mut_ptr()),
                lines.len() as u32,
                &self.text_format,
                self.pixel_size.width as f32 - margin,
                self.pixel_size.height as f32,
                &mut text_layout
            ).ok()?;
            self.buffer_layouts.insert(text_buffer.path.to_string(), TextLayout { origin, extents, layout: text_layout.unwrap() });
        }
        Ok(())
    }

    pub fn update_buffer_line_number_layout(&mut self, origin: (f32, f32), extents: (f32, f32), text_buffer: &mut TextBuffer) -> Result<()> {
        let mut line_number_string = text_buffer.get_line_number_string();
        unsafe {
            let mut text_layout = None;
            self.dwrite_factory.CreateTextLayout(
                PWSTR(line_number_string.as_mut_ptr()),
                line_number_string.len() as u32,
                &self.text_format,
                self.get_text_buffer_margin(text_buffer),
                self.pixel_size.height as f32,
                &mut text_layout
            ).ok()?;
            self.buffer_line_number_layouts.insert(text_buffer.path.to_string(), TextLayout { origin, extents, layout: text_layout.unwrap() });
        }
        Ok(())
    }

    pub fn mouse_pos_to_text_pos(&self, text_buffer: &mut TextBuffer, mouse_pos: (f32, f32)) -> Result<usize> {
        let text_layout = self.buffer_layouts.get(&text_buffer.path).unwrap();
        let adjusted_origin = self.get_text_buffer_adjusted_origin(text_buffer);
        
        let mut is_inside = BOOL::from(false);
        let mut metrics = DWRITE_HIT_TEST_METRICS::default();
        unsafe {
            text_layout.layout.HitTestPoint(
                mouse_pos.0 - adjusted_origin.0,
                mouse_pos.1 - adjusted_origin.1,
                text_buffer.get_caret_trailing_as_mut_ref(),
                &mut is_inside,
                &mut metrics
            ).ok()?;
        }
        Ok(metrics.textPosition as usize)
    }

    fn draw_selection_range(&self, origin: (f32, f32), text_layout: &IDWriteTextLayout, range: DWRITE_TEXT_RANGE) -> Result<()> {
        let mut hit_test_count = 0;
        unsafe {
            let error_code = text_layout.HitTestTextRange(
                range.startPosition, 
                range.length,
                origin.0,
                origin.1,
                null_mut(),
                0,
                &mut hit_test_count
            );
            assert!(error_code.0 == 0x8007007A, "HRESULT in this case is expected to error with \"ERROR_INSUFFICIENT_BUFFER\""); 

            let mut hit_tests : Vec<DWRITE_HIT_TEST_METRICS> = Vec::with_capacity(hit_test_count as usize);
            hit_tests.set_len(hit_test_count as usize);

            text_layout.HitTestTextRange(
                range.startPosition,
                range.length,
                origin.0,
                origin.1,
                hit_tests.as_mut_ptr(),
                hit_tests.len() as u32,
                &mut hit_test_count
            ).ok()?;

            self.render_target.SetAntialiasMode(D2D1_ANTIALIAS_MODE::D2D1_ANTIALIAS_MODE_ALIASED);
            hit_tests.iter().for_each(|metrics| {
                let highlight_rect = D2D_RECT_F {
                    left: metrics.left,
                    top: metrics.top,
                    right: metrics.left + metrics.width,
                    bottom: metrics.top + metrics.height
                };

                self.render_target.FillRectangle(&highlight_rect, self.theme.selection_brush.as_ref().unwrap());
            });
            self.render_target.SetAntialiasMode(D2D1_ANTIALIAS_MODE::D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        }
        Ok(())
    }

    fn get_rect_from_hit_test(&self, pos: u32, origin: (f32, f32), text_layout: &IDWriteTextLayout) -> Result<D2D_RECT_F> {
        let mut metrics = DWRITE_HIT_TEST_METRICS::default();
        let mut dummy = (0.0, 0.0);

        unsafe {
            text_layout.HitTestTextPosition(
                pos,
                false,
                &mut dummy.0,
                &mut dummy.1,
                &mut metrics,
            ).ok()?;

            Ok(D2D_RECT_F {
                left: origin.0 + metrics.left,
                top: origin.1 + metrics.top,
                right: origin.0 + metrics.left + metrics.width,
                bottom: origin.1 + metrics.top + metrics.height
            })
        }
    }

    fn draw_rounded_rect(&self, rect: &D2D_RECT_F) {
        let rounded_rect = D2D1_ROUNDED_RECT {
            rect: *rect,
            radiusX: 3.0,
            radiusY: 3.0
        };

        unsafe {
            self.render_target.DrawRoundedRectangle(
                &rounded_rect, 
                self.theme.bracket_brush.as_ref().unwrap(), 
                self.theme.bracket_rect_width, 
                None
            );
        }
    }

    fn draw_enclosing_brackets(&self, origin: (f32, f32), text_layout: &IDWriteTextLayout, enclosing_bracket_positions: [Option<usize>; 2]) -> Result<()> {
        match &enclosing_bracket_positions {
            [Some(pos1), Some(pos2)] => {
                let rect1 = self.get_rect_from_hit_test(*pos1 as u32, origin, &text_layout)?;
                let rect2 = self.get_rect_from_hit_test(*pos2 as u32, origin, &text_layout)?;

                // If the brackets are right next to eachother, draw one big rect
                if *pos2 == (*pos1 + 1) {
                    let rect = D2D_RECT_F {
                        left: rect1.left,
                        top: rect1.top,
                        right: rect2.right,
                        bottom: rect2.bottom
                    };
                    self.draw_rounded_rect(&rect);
                    return Ok(());
                }

                self.draw_rounded_rect(&rect1);
                self.draw_rounded_rect(&rect2);
            }
            [None, Some(pos)]  | [Some(pos), None] => {
                let rect = self.get_rect_from_hit_test(*pos as u32, origin, &text_layout)?;
                self.draw_rounded_rect(&rect);
            }
            [None, None] => {}
        }
        Ok(())
    }

    fn draw_line_numbers(&self, text_buffer: &mut TextBuffer) {
        let text_layout = self.buffer_line_number_layouts.get(&text_buffer.path).unwrap();

        unsafe {
            self.render_target.DrawTextLayout(
                D2D_POINT_2F {
                    x: text_layout.origin.0,
                    y: text_layout.origin.1
                },
                &text_layout.layout,
                self.theme.line_number_brush.as_ref().unwrap(),
                D2D1_DRAW_TEXT_OPTIONS::D2D1_DRAW_TEXT_OPTIONS_NONE
            );
        }
    }

    fn draw_text(&self, origin: (f32, f32), text_buffer: &mut TextBuffer, text_layout: &IDWriteTextLayout) -> Result<()> {
        unsafe {
            let lexical_highlights = text_buffer.get_lexical_highlights();
            // In case of overlap, lexical highlights trump semantic for now.
            // This is to ensure that commenting out big sections of code happen
            // instantaneously
            for (range, token_type) in lexical_highlights.highlight_tokens {
                match token_type {
                    SemanticTokenTypes::Comment      => { text_layout.SetDrawingEffect(self.theme.comment_brush.as_ref().unwrap(), range).ok()?; },
                    SemanticTokenTypes::Keyword      => { text_layout.SetDrawingEffect(self.theme.keyword_brush.as_ref().unwrap(), range).ok()?; },
                    SemanticTokenTypes::Literal      => { text_layout.SetDrawingEffect(self.theme.literal_brush.as_ref().unwrap(), range).ok()?; },
                    SemanticTokenTypes::Preprocessor => { text_layout.SetDrawingEffect(self.theme.macro_preprocessor_brush.as_ref().unwrap(), range).ok()?; },
                }
            }

            if let Some(selection_range) = text_buffer.get_selection_range() {
                self.draw_selection_range(origin, text_layout, DWRITE_TEXT_RANGE { startPosition: selection_range.start, length: selection_range.length })?;
            }
            if let Some(enclosing_bracket_ranges) = lexical_highlights.enclosing_brackets {
                self.draw_enclosing_brackets(origin, &text_layout, enclosing_bracket_ranges)?;
            }

            self.render_target.DrawTextLayout(
                D2D_POINT_2F { x: origin.0, y: origin.1 },
                text_layout,
                self.theme.text_brush.as_ref().unwrap(),
                D2D1_DRAW_TEXT_OPTIONS::D2D1_DRAW_TEXT_OPTIONS_NONE
            );
        }
        Ok(())
    }

    fn draw_caret(&self, origin: (f32, f32), text_buffer: &mut TextBuffer, text_layout: &IDWriteTextLayout) -> Result<()> {
        if let Some(caret_offset) = text_buffer.get_caret_offset() {
            let mut caret_pos: (f32, f32) = (0.0, 0.0);
            let mut metrics = DWRITE_HIT_TEST_METRICS::default();
            unsafe {
                text_layout.HitTestTextPosition(
                    caret_offset as u32,
                    text_buffer.get_caret_trailing(),
                    &mut caret_pos.0,
                    &mut caret_pos.1,
                    &mut metrics
                ).ok()?;

                let rect = D2D_RECT_F {
                    left: origin.0 + caret_pos.0 - (self.caret_width as f32 / 2.0),
                    top: origin.1 + caret_pos.1,
                    right: origin.0 + caret_pos.0 + (self.caret_width as f32 / 2.0),
                    bottom: origin.1 + caret_pos.1 + metrics.height
                };

                self.render_target.SetAntialiasMode(D2D1_ANTIALIAS_MODE::D2D1_ANTIALIAS_MODE_ALIASED);
                self.render_target.FillRectangle(&rect, self.theme.caret_brush.as_ref().unwrap());
                self.render_target.SetAntialiasMode(D2D1_ANTIALIAS_MODE::D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            }
        }
        Ok(())
    }

    pub fn draw(&self, text_buffer: &mut TextBuffer) -> Result<()> {
        unsafe {
            self.render_target.BeginDraw();

            self.render_target.SetTransform(&Matrix3x2::identity());
            self.render_target.Clear(&self.theme.background_color);

            self.draw_line_numbers(text_buffer);

            let text_layout = self.buffer_layouts.get(&text_buffer.path).unwrap();
            let margin = self.get_text_buffer_margin(text_buffer);
            let column_offset = self.get_text_buffer_column_offset(text_buffer);

            let clip_rect = D2D_RECT_F {
                left: text_layout.origin.0 + margin,
                top: text_layout.origin.1,
                right: text_layout.origin.0 + text_layout.extents.0,
                bottom: text_layout.origin.1 + text_layout.extents.1
            };
            self.render_target.PushAxisAlignedClip(&clip_rect, D2D1_ANTIALIAS_MODE::D2D1_ANTIALIAS_MODE_ALIASED);

            // Adjust origin to account for column offset and margin
            let adjusted_origin = (text_layout.origin.0 + margin - column_offset, text_layout.origin.1);

            self.draw_text(adjusted_origin, text_buffer, &text_layout.layout)?;
            self.draw_caret(adjusted_origin, text_buffer, &text_layout.layout)?;
            self.render_target.PopAxisAlignedClip();

            self.render_target.EndDraw(null_mut(), null_mut()).ok()?;
        }
        Ok(())
    }

    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        self.pixel_size.width = width;
        self.pixel_size.height = height;
        unsafe {
            self.render_target.Resize(&self.pixel_size).ok()?;
        }
        let (font_width, font_height) = get_font_width_and_height(&self.dwrite_factory, &self.text_format).unwrap();
        self.font_width = font_width;
        self.font_height = font_height;
        Ok(())
    }
}
