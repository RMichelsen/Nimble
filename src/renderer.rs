use crate::{
    settings,
    buffer::TextBuffer,
    theme::Theme,
    status_bar::StatusBar,
    file_tree::FileTree,
    lsp_structs::SemanticTokenTypes
};

use std::{
    ptr::null_mut,
    mem::MaybeUninit,
    ffi::OsStr,
    iter::once,
    os::windows::ffi::OsStrExt
};
use winapi::{
    ctypes::c_void,
    Interface,
    um::{
        dcommon::{D2D1_ALPHA_MODE_UNKNOWN, D2D1_PIXEL_FORMAT},
        dwrite::{
            DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, 
            IDWriteTextLayout, DWRITE_WORD_WRAPPING_NO_WRAP,
            DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL,
            DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_PARAGRAPH_ALIGNMENT_NEAR,
            DWRITE_TEXT_RANGE, DWRITE_HIT_TEST_METRICS
        },
        d2d1::{
            ID2D1Factory, ID2D1HwndRenderTarget, D2D1CreateFactory,
            ID2D1Brush, D2D1_LAYER_PARAMETERS,
            D2D1_LAYER_OPTIONS_INITIALIZE_FOR_CLEARTYPE,
            D2D1_PRESENT_OPTIONS_NONE, D2D1_ROUNDED_RECT,
            D2D1_POINT_2F, D2D1_MATRIX_3X2_F, D2D1_SIZE_U, D2D1_RECT_F,
            D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_FEATURE_LEVEL_DEFAULT,
            D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT,
            D2D1_DRAW_TEXT_OPTIONS_CLIP,
            D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_USAGE_NONE,
            D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_PROPERTIES,
            D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_ANTIALIAS_MODE_ALIASED,
            D2D1_ANTIALIAS_MODE_PER_PRIMITIVE
        },
        unknwnbase::IUnknown,
        winuser::{GetClientRect, GetDpiForWindow}
    },
    shared::{
        dxgiformat::DXGI_FORMAT_UNKNOWN,
        windef::{RECT, HWND}
    }
};

#[macro_export]
#[cfg(debug_assertions)]
macro_rules! hr_ok {
    ($e:expr) => {
        assert!($e == 0, "Call returned error code HRESULT: 0x{:x}", $e as u32)
    }
}

#[macro_export]
#[cfg(not(debug_assertions))]
macro_rules! hr_ok {
    ($e:expr) => {
        std::convert::identity($e)
    }
}

const IDENTITY_MATRIX: D2D1_MATRIX_3X2_F = D2D1_MATRIX_3X2_F { matrix: [[1.0, 0.0], [0.0, 1.0], [0.0, 0.0]] };

pub trait RenderableTextRegion {
    fn get_rect(&self) -> D2D1_RECT_F;
    fn get_origin(&self) -> (f32, f32);
    fn get_layout(&mut self) -> *mut IDWriteTextLayout;
    fn resize(&mut self, origin: (f32, f32), extents: (f32, f32));
}

pub struct TextRenderer {
    dpi_scale: f32,
    pub pixel_size: D2D1_SIZE_U,
    pub font_size: f32,
    pub font_height: f32,
    pub font_width: f32,
    font_name: Vec<u16>,
    font_locale: Vec<u16>,

    theme: Theme,

    pub write_factory: *mut IDWriteFactory,
    pub text_format: *mut IDWriteTextFormat,
    
    factory: *mut ID2D1Factory,
    target: *mut ID2D1HwndRenderTarget
}

impl TextRenderer {
    pub fn new(hwnd: HWND, font: &str, font_size: f32) -> Self {
        let mut renderer = Self {
            dpi_scale: 0.0,
            pixel_size: D2D1_SIZE_U {
                width: 0,
                height: 0
            },
            font_size,
            font_height: 0.0,
            font_width: 0.0,
            font_name: Vec::new(),
            font_locale: Vec::new(),

            theme: Theme::default(),
                
            write_factory: null_mut(),
            text_format: null_mut(),

            factory: null_mut(),
            target: null_mut()
        };

        unsafe {
            hr_ok!(
                D2D1CreateFactory(
                    D2D1_FACTORY_TYPE_SINGLE_THREADED, 
                    &ID2D1Factory::uuidof(), null_mut(), 
                    (&mut renderer.factory as *mut *mut _) as *mut *mut c_void
                )
            );

            let dpi = GetDpiForWindow(hwnd);
            renderer.dpi_scale = dpi as f32 / 96.0;

            // Scale the font size to fit the dpi
            renderer.font_size *= renderer.dpi_scale;

            let mut rect_uninit = MaybeUninit::<RECT>::uninit();
            GetClientRect(hwnd, rect_uninit.as_mut_ptr());
            let rect = rect_uninit.assume_init();
            renderer.pixel_size = D2D1_SIZE_U {
                width: (rect.right - rect.left) as u32,
                height: (rect.bottom - rect.top) as u32
            };

            let target_props = D2D1_RENDER_TARGET_PROPERTIES {
                _type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_UNKNOWN,
                    alphaMode: D2D1_ALPHA_MODE_UNKNOWN
                },
                dpiX: 96.0,
                dpiY: 96.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT
            };

            let hwnd_props = D2D1_HWND_RENDER_TARGET_PROPERTIES {
                hwnd,
                pixelSize: renderer.pixel_size,
                presentOptions: D2D1_PRESENT_OPTIONS_NONE
            };

            hr_ok!((*renderer.factory).CreateHwndRenderTarget(&target_props, &hwnd_props, &mut renderer.target)); 

            renderer.theme = Theme::new_default(renderer.target);

            hr_ok!(
                DWriteCreateFactory(
                    DWRITE_FACTORY_TYPE_SHARED, 
                    &IDWriteFactory::uuidof(), 
                    (&mut renderer.write_factory as *mut *mut _) as *mut *mut IUnknown
                )
            );

            renderer.font_name = OsStr::new(font).encode_wide().chain(once(0)).collect();
            renderer.font_locale = OsStr::new("en-us").encode_wide().chain(once(0)).collect();
            
            renderer.update_text_format(0.0);
        }

        renderer
    }

    pub fn layer_params(origin: (f32, f32), size: (f32, f32)) -> D2D1_LAYER_PARAMETERS {
        D2D1_LAYER_PARAMETERS {
            contentBounds: D2D1_RECT_F {
                left: origin.0,
                right: origin.0 + size.0,
                top: origin.1,
                bottom: origin.1 + size.1
            },
            geometricMask: null_mut(),
            maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
            maskTransform: IDENTITY_MATRIX,
            opacity: 1.0,
            opacityBrush: null_mut(),
            layerOptions: D2D1_LAYER_OPTIONS_INITIALIZE_FOR_CLEARTYPE
        }
    }

    pub fn update_text_format(&mut self, font_size_delta: f32) {
        if (self.font_size + font_size_delta) > 0.0 {
            self.font_size += font_size_delta;
        }

        unsafe {
            // Release the old text format
            if !self.text_format.is_null() {
                (*self.text_format).Release();
            }

            hr_ok!((*self.write_factory).CreateTextFormat(
                self.font_name.as_ptr(),
                null_mut(),
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                self.font_size,
                self.font_locale.as_ptr(),
                &mut self.text_format
            ));
            hr_ok!((*self.text_format).SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING));
            hr_ok!((*self.text_format).SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR));
            hr_ok!((*self.text_format).SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP));
        }

        self.update_font_metrics();
    }

    fn update_font_metrics(&mut self) {
        static GLYPH_CHAR: u16 = 0x0061;
        unsafe {
            let mut test_text_layout: *mut IDWriteTextLayout = null_mut();
            hr_ok!((*self.write_factory).CreateTextLayout(
                &GLYPH_CHAR,
                1,
                self.text_format,
                0.0,
                0.0,
                &mut test_text_layout
            ));

            let mut metrics_uninit = MaybeUninit::<DWRITE_HIT_TEST_METRICS>::uninit();
            let mut dummy: (f32, f32) = (0.0, 0.0);
            hr_ok!((*test_text_layout).HitTestTextPosition(
                0,
                0,
                &mut dummy.0,
                &mut dummy.1,
                metrics_uninit.as_mut_ptr()
            ));
            let metrics = metrics_uninit.assume_init();
            (*test_text_layout).Release();

            self.font_width = metrics.width;
            self.font_height = metrics.height;

            hr_ok!((*self.text_format).SetIncrementalTabStop(self.font_width * settings::NUMBER_OF_SPACES_PER_TAB as f32));
        }
    }

    fn draw_selection_range(&self, origin: (f32, f32), text_layout: *mut IDWriteTextLayout, range: DWRITE_TEXT_RANGE) {
        let mut hit_test_count = 0;

        unsafe {
            let hr: i32 = (*text_layout).HitTestTextRange(
                        range.startPosition, 
                        range.length,
                        origin.0,
                        origin.1,
                        null_mut(),
                        0,
                        &mut hit_test_count
                    );
            assert!((hr as u32) == 0x8007007A, "HRESULT in this case is expected to error with \"ERROR_INSUFFICIENT_BUFFER\""); 

            let mut hit_tests : Vec<DWRITE_HIT_TEST_METRICS> = Vec::with_capacity(hit_test_count as usize);
            hit_tests.set_len(hit_test_count as usize);

            hr_ok!(
                (*text_layout).HitTestTextRange(
                    range.startPosition,
                    range.length,
                    origin.0,
                    origin.1,
                    hit_tests.as_mut_ptr(),
                    hit_tests.len() as u32,
                    &mut hit_test_count
                )
            );

            (*self.target).SetAntialiasMode(D2D1_ANTIALIAS_MODE_ALIASED);
            hit_tests.iter().for_each(|metrics| {

                let highlight_rect = D2D1_RECT_F {
                    left: metrics.left,
                    top: metrics.top,
                    right: metrics.left + metrics.width,
                    bottom: metrics.top + metrics.height
                };

                (*self.target).FillRectangle(&highlight_rect, self.theme.selection_brush as *mut ID2D1Brush);

            });
            (*self.target).SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);

        }
    }

    fn get_rect_from_hit_test(&self, pos: u32, origin: (f32, f32), text_layout: *mut IDWriteTextLayout) -> D2D1_RECT_F {
        let mut metrics_uninit = MaybeUninit::<DWRITE_HIT_TEST_METRICS>::uninit();
        let mut dummy = (0.0, 0.0);

        unsafe {
            hr_ok!((*text_layout).HitTestTextPosition(
                pos,
                false as i32,
                &mut dummy.0,
                &mut dummy.1,
                metrics_uninit.as_mut_ptr(),
            ));
            let metrics = metrics_uninit.assume_init();

            D2D1_RECT_F {
                left: origin.0 + metrics.left,
                top: origin.1 + metrics.top,
                right: origin.0 + metrics.left + metrics.width,
                bottom: origin.1 + metrics.top + metrics.height
            }
        }
    }

    fn draw_rounded_rect(&self, rect: &D2D1_RECT_F) {
        let rounded_rect = D2D1_ROUNDED_RECT {
            rect: *rect,
            radiusX: 3.0,
            radiusY: 3.0
        };

        unsafe {
            (*self.target).DrawRoundedRectangle(
                &rounded_rect, 
                self.theme.bracket_brush as *mut ID2D1Brush, 
                self.theme.bracket_rect_width, 
                null_mut()
            );
        }
    }

    fn draw_enclosing_brackets(&self, origin: (f32, f32), text_layout: *mut IDWriteTextLayout, enclosing_bracket_positions: [Option<usize>; 2]) {
        match &enclosing_bracket_positions {
            [Some(pos1), Some(pos2)] => {
                let rect1 = self.get_rect_from_hit_test(*pos1 as u32, origin, text_layout);
                let rect2 = self.get_rect_from_hit_test(*pos2 as u32, origin, text_layout);

                // If the brackets are right next to eachother, draw one big rect
                if *pos2 == (*pos1 + 1) {
                    let rect = D2D1_RECT_F {
                        left: rect1.left,
                        top: rect1.top,
                        right: rect2.right,
                        bottom: rect2.bottom
                    };
                    self.draw_rounded_rect(&rect);
                    return;
                }

                self.draw_rounded_rect(&rect1);
                self.draw_rounded_rect(&rect2);
            }
            [None, Some(pos)]  | [Some(pos), None] => {
                let rect = self.get_rect_from_hit_test(*pos as u32, origin, text_layout);
                self.draw_rounded_rect(&rect);
            }
            [None, None] => {}
        }
    }

    fn draw_renderable_region<T>(&self, region: &mut T, background_brush: *mut ID2D1Brush, text_brush: *mut ID2D1Brush) where T: RenderableTextRegion {
        unsafe {
            let rect = region.get_rect();
            (*self.target).FillRectangle(&rect, background_brush as *mut ID2D1Brush);

            let (origin_x, origin_y) = region.get_origin();
            let layout = region.get_layout();
            (*self.target).DrawTextLayout(
                D2D1_POINT_2F {
                    x: origin_x,
                    y: origin_y
                },
                layout,
                text_brush as *mut ID2D1Brush,
                D2D1_DRAW_TEXT_OPTIONS_CLIP | D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT
            );
        }
    }

    pub fn draw(&self, text_buffer: Option<&mut TextBuffer>, status_bar: &mut StatusBar, file_tree: &mut FileTree, draw_caret: bool) {

        unsafe {
            (*self.target).BeginDraw();

            (*self.target).SetTransform(&IDENTITY_MATRIX);
            (*self.target).Clear(&self.theme.background_color);

            if let Some(buffer) = text_buffer {
                // Push the line numbers layer params before drawing
                let (line_numbers_layout, line_numbers_layer_params) = buffer.get_line_numbers_layout();
                (*self.target).PushLayer(&line_numbers_layer_params, null_mut());
                (*self.target).DrawTextLayout(
                    D2D1_POINT_2F { 
                        x: buffer.line_numbers_origin.0,
                        y: buffer.line_numbers_origin.1
                    },
                    line_numbers_layout,
                    self.theme.line_number_brush as *mut ID2D1Brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE
                );
                (*self.target).PopLayer();

                // Push the text layer params before drawing
                let (text_layout, text_layer_params) = buffer.get_text_layout();
                (*self.target).PushLayer(&text_layer_params, null_mut());

                let lexical_highlights = buffer.get_lexical_highlights();
                let semantic_highlights = buffer.get_semantic_highlights();
                // In case of overlap, lexical highlights trump semantic for now.
                // This is to ensure that commenting out big sections of code happen
                // instantaneously
                for (range, token_type) in semantic_highlights.into_iter().chain(lexical_highlights.highlight_tokens) {
                    match token_type {
                        SemanticTokenTypes::None | SemanticTokenTypes::Variable          
                                                            => {},
                        SemanticTokenTypes::Function          => { hr_ok!((*text_layout).SetDrawingEffect(self.theme.function_brush as *mut IUnknown, range)); },
                        SemanticTokenTypes::Method            => { hr_ok!((*text_layout).SetDrawingEffect(self.theme.method_brush as *mut IUnknown, range)); },
                        SemanticTokenTypes::Class             => { hr_ok!((*text_layout).SetDrawingEffect(self.theme.class_brush as *mut IUnknown, range)); },
                        SemanticTokenTypes::Enum              => { hr_ok!((*text_layout).SetDrawingEffect(self.theme.enum_brush as *mut IUnknown, range)); },
                        SemanticTokenTypes::Comment           => { hr_ok!((*text_layout).SetDrawingEffect(self.theme.comment_brush as *mut IUnknown, range)); },
                        SemanticTokenTypes::Keyword           => { hr_ok!((*text_layout).SetDrawingEffect(self.theme.keyword_brush as *mut IUnknown, range)); },
                        SemanticTokenTypes::Literal           => { hr_ok!((*text_layout).SetDrawingEffect(self.theme.literal_brush as *mut IUnknown, range)); },
                        SemanticTokenTypes::Macro | SemanticTokenTypes::Preprocessor             
                                                            => { hr_ok!((*text_layout).SetDrawingEffect(self.theme.macro_preprocessor_brush as *mut IUnknown, range)); },
                        SemanticTokenTypes::Primitive         => { hr_ok!((*text_layout).SetDrawingEffect(self.theme.primitive_brush as *mut IUnknown, range)); }
                    }
                }

                let view_origin = buffer.get_view_origin();
                if let Some(selection_range) = buffer.get_selection_range() {
                    self.draw_selection_range(view_origin, text_layout, selection_range);
                }
                if let Some(enclosing_bracket_ranges) = lexical_highlights.enclosing_brackets {
                    self.draw_enclosing_brackets(view_origin, text_layout, enclosing_bracket_ranges)
                }

                (*self.target).DrawTextLayout(
                    D2D1_POINT_2F { 
                        x: view_origin.0,
                        y: view_origin.1
                    },
                    text_layout,
                    self.theme.text_brush as *mut ID2D1Brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE
                );

                if draw_caret {
                    if let Some(rect) = buffer.get_caret_rect() {
                        (*self.target).SetAntialiasMode(D2D1_ANTIALIAS_MODE_ALIASED);
                        (*self.target).FillRectangle(&rect, self.theme.caret_brush as *mut ID2D1Brush);
                        (*self.target).SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
                    }
                }
                (*self.target).PopLayer();
            }

            self.draw_renderable_region(status_bar, self.theme.status_bar_brush as *mut ID2D1Brush, self.theme.text_brush as *mut ID2D1Brush);

            self.draw_renderable_region(file_tree, self.theme.status_bar_brush as *mut ID2D1Brush, self.theme.text_brush as *mut ID2D1Brush);

            if let Some(hover_rect) = file_tree.hovered_line_rect {
                (*self.target).FillRectangle(&hover_rect, self.theme.selection_brush as *mut ID2D1Brush);
            }

            // if let Some(file_tree_hover_selection) = file_tree.get_hover_line_selection() {
            //     self.draw_selection_range(file_tree.origin, file_tree.get_layout(), file_tree_hover_selection);
            // }

            (*self.target).EndDraw(null_mut(), null_mut());
        }
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.pixel_size.width = width;
        self.pixel_size.height = height;
        unsafe {
            (*self.target).Resize(&self.pixel_size);
        }
        self.update_font_metrics();
    }
}
