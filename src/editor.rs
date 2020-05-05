use std::{
    collections::HashMap,
    str,
    rc::Rc, 
    cell::RefCell,
    path::Path
};
use winapi::shared::windef::HWND;
use winapi::um::winuser::{VK_LEFT, VK_RIGHT, VK_UP, VK_DOWN, VK_TAB, VK_RETURN, VK_DELETE, VK_BACK, SendMessageW};

use crate::WM_REGION_CHANGED;
use crate::settings::{SCROLL_LINES_PER_MOUSEMOVE, SCROLL_LINES_PER_ROLL, 
    NUMBER_OF_SPACES_PER_TAB, SCROLL_ZOOM_DELTA};
use crate::renderer::{TextRenderer, RenderableTextRegion};
use crate::lsp_client::{LSPClient, LSPRequestType};
use crate::lsp_structs::{GenericNotification, GenericRequest, GenericResponse, 
    DidChangeNotification, ResponseError, SemanticTokenResult, ErrorCodes};
use crate::language_support::{CPP_FILE_EXTENSIONS, CPP_LSP_SERVER, CPP_LANGUAGE_IDENTIFIER, 
    RUST_LSP_SERVER, RUST_FILE_EXTENSIONS, RUST_LANGUAGE_IDENTIFIER};
use crate::buffer::{TextBuffer, SelectionMode, MouseSelectionMode};
use crate::status_bar::StatusBar;
use crate::file_tree::FileTree;

type MousePos = (f32, f32);
type ShiftDown = bool;
type CtrlDown = bool;

#[derive(PartialEq)]
pub enum EditorCommand {
    CaretVisible,
    CaretInvisible,
    ScrollUp(CtrlDown),
    ScrollDown(CtrlDown),
    LeftClick(MousePos, ShiftDown),
    LeftDoubleClick(MousePos),
    LeftRelease,
    MouseMove(MousePos),
    KeyPressed(i32, ShiftDown, CtrlDown),
    CharInsert(u16),
    LSPClientCrash(&'static str)
}

#[derive(Copy, Clone, Debug)]
pub struct EditorLayout {
    pub layout_origin: (f32, f32),
    pub layout_extents: (f32, f32),
    pub buffer_origin: (f32, f32),
    pub buffer_extents: (f32, f32),
    pub status_bar_origin: (f32, f32),
    pub status_bar_extents: (f32, f32),
    pub file_tree_origin: (f32, f32),
    pub file_tree_extents: (f32, f32)
}
impl Default for EditorLayout {
    fn default() -> Self {
        Self {
            layout_origin: (0.0, 0.0),
            layout_extents: (0.0, 0.0),
            buffer_origin: (0.0, 0.0),
            buffer_extents: (0.0, 0.0),
            status_bar_origin: (0.0, 0.0),
            status_bar_extents: (0.0, 0.0),
            file_tree_origin: (0.0, 0.0),
            file_tree_extents: (0.0, 0.0)
        }
    }
}
impl EditorLayout {
    pub fn new(width: f32, height: f32, font_height: f32) -> Self {
        let file_tree_width = width / 7.5;
        Self {
            layout_origin: (0.0, 0.0),
            layout_extents: (width, height),
            buffer_origin: (file_tree_width, 0.0),
            buffer_extents: (width - file_tree_width, height - font_height),
            status_bar_origin: (0.0, height - font_height),
            status_bar_extents: (width, font_height),
            file_tree_origin: (0.0, 0.0),
            file_tree_extents: (file_tree_width, height - font_height)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RegionType {
    Display = 0,
    Text = 1,
    ResizableBorder = 2,
    Unknown = 3
}

impl RegionType {
    pub fn from_usize(uint: usize) -> Self {
        match uint {
            0 => Self::Display,
            1 => Self::Text,
            2 => Self::ResizableBorder,
            _ => Self::Unknown
        }
    }

    pub fn to_usize(region_type: Self) -> usize {
        match region_type {
            Self::Display => 0,
            Self::Text => 1,
            Self::ResizableBorder => 2,
            Self::Unknown => 3
        }
    }
}

pub struct Editor {
    hwnd: HWND,
    renderer: Rc<RefCell<TextRenderer>>,
    layout: EditorLayout,

    lsp_client: Option<LSPClient>,

    status_bar: StatusBar,
    file_tree: FileTree,

    buffers: HashMap<String, TextBuffer>,
    current_buffer: String,

    region_type: RegionType,

    mouse_pos: (f32, f32),
    mouse_pos_captured: bool,
    force_visible_caret_timer: u32,
    caret_is_visible: bool
}

impl Editor {
    pub fn new(hwnd: HWND) -> Self {
        let renderer = Rc::new(RefCell::new(TextRenderer::new(hwnd, "Fira Code Retina", 20.0)));

        let layout = EditorLayout::new(
            renderer.borrow().pixel_size.width as f32,
            renderer.borrow().pixel_size.height as f32,
            renderer.borrow().font_height);

        Self {
            hwnd,
            renderer: renderer.clone(),
            layout,

            lsp_client: None,

            status_bar: StatusBar::new(layout.status_bar_origin, layout.status_bar_extents, renderer.clone()),
            file_tree: FileTree::new("C:/", layout.file_tree_origin, layout.file_tree_extents, renderer.clone()),

            buffers: HashMap::new(),
            current_buffer: "".to_owned(),

            region_type: RegionType::Display,
            mouse_pos: (0.0, 0.0),
            mouse_pos_captured: false,
            force_visible_caret_timer: 0,
            caret_is_visible: true
        }
    }

    pub fn open_file(&mut self, path: &str) {
        let file_prefix = "file:///".to_owned();
        let os_path = Path::new(path);
        let extension = os_path.extension().unwrap().to_str().unwrap();

        let language_identifier = 
        if CPP_FILE_EXTENSIONS.contains(&extension) {
            CPP_LANGUAGE_IDENTIFIER
        }
        else if RUST_FILE_EXTENSIONS.contains(&extension) {
            RUST_LANGUAGE_IDENTIFIER
        }
        else {
            ""
        };

        self.buffers.insert(
            file_prefix.clone() + path,
            TextBuffer::new(
                path,
                language_identifier,
                self.layout.buffer_origin, 
                self.layout.buffer_extents, 
                self.renderer.clone()
            )
        );
        self.current_buffer = file_prefix.clone() + path;

        // If the LSP Client is not yet running, create an instance
        // we then return since we will handle the open file request
        // once the LSP Client is actually initialized.
        match &self.lsp_client {
            None if CPP_FILE_EXTENSIONS.contains(&extension) => {
                self.lsp_client = Some(LSPClient::new(self.hwnd, CPP_LSP_SERVER));
                self.lsp_client.as_mut().unwrap().send_initialize_request(path.to_owned());
                return;
            },
            None if RUST_FILE_EXTENSIONS.contains(&extension) => {
                self.lsp_client = Some(LSPClient::new(self.hwnd, RUST_LSP_SERVER));
                self.lsp_client.as_mut().unwrap().send_initialize_request(path.to_owned());
                return;
            },
            _ => {}
        }

        let lsp_client = self.lsp_client.as_mut().unwrap();
        let text = std::fs::read_to_string(os_path).unwrap();
        lsp_client.send_did_open_notification(file_prefix.clone() + path, language_identifier.to_owned(), text);
        lsp_client.send_semantic_token_request(file_prefix + path);
    }

    pub fn draw(&mut self) {
        if let Some(buffer) = self.buffers.get_mut(&self.current_buffer) {
            self.renderer.borrow().draw(buffer, &mut self.status_bar, &mut self.file_tree, self.caret_is_visible);
        }
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.renderer.borrow_mut().resize(width, height);

        self.layout = EditorLayout::new(
            self.renderer.borrow().pixel_size.width as f32,
            self.renderer.borrow().pixel_size.height as f32,
            self.renderer.borrow().font_height);

        self.status_bar.resize(self.layout.status_bar_origin, self.layout.status_bar_extents);
        self.file_tree.resize(self.layout.file_tree_origin, self.layout.file_tree_extents);

        for buffer in self.buffers.values_mut() {
            buffer.on_refresh_metrics(
                self.layout.buffer_origin,
                self.layout.buffer_extents
            );
        }
    }

    pub fn capture_mouse(&mut self) {
        self.mouse_pos_captured = true;
    }

    pub fn release_mouse(&mut self) {
        self.mouse_pos_captured = false;
    }

    pub fn selection_active(&self) -> bool {
        if let Some(buffer) = self.buffers.get(&self.current_buffer) {
            return buffer.currently_selecting;
        }
        false
    }

    pub fn mouse_left_window(&mut self) {
        self.region_type = RegionType::Unknown;
    }

    fn handle_response_error(&mut self, request_type: LSPRequestType, response_error: &ResponseError) {
        match request_type {
            LSPRequestType::InitializationRequest(_) => {},
            LSPRequestType::SemanticTokensRequest(uri) => {
                // If the semantic token request fails
                // due to content changed, send a new one
                if ErrorCodes::from_i64((*response_error).code) == ErrorCodes::ContentModified {
                    if let Some(lsp_client) = self.lsp_client.as_mut() {
                        lsp_client.send_semantic_token_request(uri);
                    }
                }
            }
        }
    }

    fn handle_response_success(&mut self, request_type: LSPRequestType, result_value: serde_json::Value) {
        if let Some(lsp_client) = self.lsp_client.as_mut() {
            match request_type {
                LSPRequestType::InitializationRequest(path) => {
                    // Send init notification
                    lsp_client.send_initialized_notification();
    
                    // Then open the file that triggered the LSP creation
                    let file_prefix = "file:///".to_owned();
                    let os_path = Path::new(path.as_str());
                    let extension = os_path.extension().unwrap().to_str().unwrap();
            
                    let language_identifier = 
                    if CPP_FILE_EXTENSIONS.contains(&extension) {
                        CPP_LANGUAGE_IDENTIFIER
                    }
                    else if RUST_FILE_EXTENSIONS.contains(&extension) {
                        RUST_LANGUAGE_IDENTIFIER
                    }
                    else {
                        ""
                    };
                    let text = std::fs::read_to_string(os_path).unwrap();
                    lsp_client.send_did_open_notification(file_prefix.clone() + path.as_str(), language_identifier.to_owned(), text);
                    lsp_client.send_semantic_token_request(file_prefix + path.as_str());
                },
                LSPRequestType::SemanticTokensRequest(uri) => {
                    // Get the buffer for which the semantic token request was issued
                    let buffer = self.buffers.get_mut(&uri).unwrap();
    
                    // Update the semantic tokens of the buffer if they are updated
                    if let Ok(result) = serde_json::from_value::<SemanticTokenResult>(result_value) {
                        buffer.update_semantic_tokens(result.data);
                    }
                }
            }
        }
    }

    pub fn process_language_server_response(&mut self, message: &str) {
        if let Ok(response) = serde_json::from_str::<GenericResponse>(message) {
            let response_id = match response.id {
                serde_json::Value::Number(x) => x.as_i64().unwrap(),
                serde_json::Value::String(x) => x.parse::<i64>().unwrap(),
                _ => {
                    println!("Unrecognized response ID from language server");
                    -1
                }
            };

            if let Some(lsp_client) = self.lsp_client.as_mut() {
                let request_type = lsp_client.request_types[response_id as usize].clone();

                // Handle any errors
                if let Some(response_error) = response.error {
                    self.handle_response_error(request_type, &response_error)
                }
                // Spec says result is guaranteed to be Some(), when there is no error
                // rust-analyzer doesn't seem to honor this so we have to check it
                else if let Some(response_result) = response.result {
                    self.handle_response_success(request_type, response_result);
                }
            }
        }
        else if let Ok(_) = serde_json::from_str::<GenericNotification>(message) {
            // Atm we don't handle requests
        }
        else if let Ok(_) = serde_json::from_str::<GenericRequest>(message) {
            // Atm we don't handle requests
        }
    }

    fn process_document_change(did_change_notification: &DidChangeNotification, buffer: &mut TextBuffer, lsp_client: &mut LSPClient) {
        // rust-analyzer only supports full change notifications
        match buffer.language_identifier {
            CPP_LANGUAGE_IDENTIFIER => {
                lsp_client.send_did_change_notification(did_change_notification);
                lsp_client.send_semantic_token_request(buffer.get_uri());
            },
            RUST_LANGUAGE_IDENTIFIER => {
                let full_did_change_notification = buffer.get_full_did_change_notification();
                lsp_client.send_did_change_notification(&full_did_change_notification);
                lsp_client.send_semantic_token_request(buffer.get_uri());
            },
            _ => {}
        }
    }

    fn force_caret_visible(caret_is_visible: &mut bool, caret_timer: &mut u32) {
        if *caret_is_visible {
            *caret_timer = 1;
        }
        else {
            *caret_is_visible = true;
            *caret_timer = 2;
        }
    }

    fn change_font_size(zoom_delta: f32, layout: &mut EditorLayout, renderer: &mut TextRenderer) {
        renderer.update_text_format(zoom_delta);

        *layout = EditorLayout::new(
            renderer.pixel_size.width as f32,
            renderer.pixel_size.height as f32,
            renderer.font_height);
    }

    fn inside_region(pos: (f32, f32), origin: (f32, f32), extents: (f32, f32)) -> bool {
        let horizontal_range = origin.0..(origin.0 + extents.0);
        let vertical_range = origin.1..(origin.1 + extents.1);
        horizontal_range.contains(&pos.0) && vertical_range.contains(&pos.1)
    }

    fn execute_buffer_command(&mut self, cmd: &EditorCommand) {
        if let Some(buffer) = self.buffers.get_mut(&self.current_buffer) {
            match *cmd {
                EditorCommand::CaretVisible | EditorCommand::CaretInvisible if self.force_visible_caret_timer > 0 => {
                    self.force_visible_caret_timer = self.force_visible_caret_timer.saturating_sub(1);
                    self.caret_is_visible = true;
                },
                EditorCommand::CaretVisible => self.caret_is_visible = true,
                EditorCommand::CaretInvisible => self.caret_is_visible = false,
                EditorCommand::ScrollUp(ctrl_down) => {
                    match ctrl_down {
                        true => {
                            Self::change_font_size(SCROLL_ZOOM_DELTA, &mut self.layout, &mut *self.renderer.borrow_mut());
                            buffer.on_refresh_metrics(
                                self.layout.buffer_origin,
                                self.layout.buffer_extents
                            );
                        },
                        false => buffer.scroll_up(SCROLL_LINES_PER_ROLL)
                    }
                },
                EditorCommand::ScrollDown(ctrl_down) => {
                    match ctrl_down {
                        true => {
                            Self::change_font_size(-SCROLL_ZOOM_DELTA, &mut self.layout, &mut *self.renderer.borrow_mut());
                            buffer.on_refresh_metrics(
                                self.layout.buffer_origin,
                                self.layout.buffer_extents
                            );
                        }
                        false => buffer.scroll_down(SCROLL_LINES_PER_ROLL)
                    }
                },
                EditorCommand::LeftClick(mouse_pos, shift_down) => {
                    buffer.left_click(mouse_pos, shift_down);
                    Self::force_caret_visible(&mut self.caret_is_visible, &mut self.force_visible_caret_timer);
                },
                EditorCommand::LeftDoubleClick(mouse_pos) => {
                    buffer.left_double_click(mouse_pos);
                    Self::force_caret_visible(&mut self.caret_is_visible, &mut self.force_visible_caret_timer);
                },
                EditorCommand::LeftRelease => buffer.left_release(),
                EditorCommand::MouseMove(mouse_pos) => {
                    if mouse_pos.1 > (self.layout.layout_origin.1 + self.layout.layout_extents.1) {
                        buffer.scroll_down(SCROLL_LINES_PER_MOUSEMOVE);
                    }
                    else if mouse_pos.1 < self.layout.layout_origin.1 {
                        buffer.scroll_up(SCROLL_LINES_PER_MOUSEMOVE);
                    }
                    if mouse_pos.0 > (self.layout.layout_origin.0 + self.layout.layout_extents.0) {
                        buffer.scroll_right(SCROLL_LINES_PER_MOUSEMOVE);
                    }
                    else if mouse_pos.0 < self.layout.layout_origin.0 {
                        buffer.scroll_left(SCROLL_LINES_PER_MOUSEMOVE);
                    }
                    buffer.set_mouse_selection(MouseSelectionMode::Move, mouse_pos);
                },
                EditorCommand::KeyPressed(key, shift_down, ctrl_down) => { 
                    match (key, ctrl_down) {
                        (VK_LEFT, false)   => buffer.move_left(shift_down),
                        (VK_LEFT, true)    => buffer.move_left_by_word(shift_down),
                        (VK_RIGHT, false)  => buffer.move_right(shift_down),
                        (VK_RIGHT, true)   => buffer.move_right_by_word(shift_down),
                        (VK_DOWN, _)       => buffer.set_selection(SelectionMode::Down, 1, shift_down),
                        (VK_UP, _)         => buffer.set_selection(SelectionMode::Up, 1, shift_down),
                        (VK_TAB, _)        => {
                            let did_change_notification = buffer.insert_chars(" ".repeat(NUMBER_OF_SPACES_PER_TAB).as_str());
                            if let Some(lsp_client) = self.lsp_client.as_mut() {
                                Self::process_document_change(&did_change_notification, buffer, lsp_client);
                            }
                        },
                        (VK_RETURN, false) => {
                            let did_change_notification = buffer.insert_newline();
                            if let Some(lsp_client) = self.lsp_client.as_mut() {
                                Self::process_document_change(&did_change_notification, buffer, lsp_client);
                            }
                        },
                        (VK_DELETE, false) => {
                            let did_change_notification = buffer.delete_right();
                            if let Some(lsp_client) = self.lsp_client.as_mut() {
                                Self::process_document_change(&did_change_notification, buffer, lsp_client);
                            }
                        },
                        (VK_DELETE, true) => {
                            let did_change_notification = buffer.delete_right_by_word();
                            if let Some(lsp_client) = self.lsp_client.as_mut() {
                                Self::process_document_change(&did_change_notification, buffer, lsp_client);
                            }
                        },
                        (VK_BACK, false) => {
                            let did_change_notification = buffer.delete_left();
                            if let Some(lsp_client) = self.lsp_client.as_mut() {
                                Self::process_document_change(&did_change_notification, buffer, lsp_client);
                            }
                        },
                        (VK_BACK, true) => {
                            let did_change_notification = buffer.delete_left_by_word();
                            if let Some(lsp_client) = self.lsp_client.as_mut() {
                                Self::process_document_change(&did_change_notification, buffer, lsp_client);
                            }
                        },
                        // CTRL+A (Select all)
                        (0x41, true) => {
                            buffer.select_all();
                        }
                        // CTRL+C (Copy)
                        (0x43, true) => {
                            buffer.copy_selection(self.hwnd);
                        },
                        // CTRL+X (Cut)
                        (0x58, true) => {
                            let did_change_notification = buffer.cut_selection(self.hwnd);
                            if let Some(lsp_client) = self.lsp_client.as_mut() {
                                Self::process_document_change(&did_change_notification, buffer, lsp_client);
                            }
                        },
                        // CTRL+V (Paste)
                        (0x56, true) => {
                            let did_change_notification = buffer.paste(self.hwnd);
                            if let Some(lsp_client) = self.lsp_client.as_mut() {
                                match did_change_notification {
                                    None => {},
                                    Some(notification) => Self::process_document_change(&notification, buffer, lsp_client)
                                }
                            }
                        }
                        _ => {}
                    }
                    Self::force_caret_visible(&mut self.caret_is_visible, &mut self.force_visible_caret_timer);
                }
                EditorCommand::CharInsert(character) => {
                    let did_change_notification = buffer.insert_char(character);
                    if let Some(lsp_client) = self.lsp_client.as_mut() {
                        Self::process_document_change(&did_change_notification, buffer, lsp_client);
                    }
                    Self::force_caret_visible(&mut self.caret_is_visible, &mut self.force_visible_caret_timer);
                }
                EditorCommand::LSPClientCrash(client) => {
                    println!("The {} language server has crashed!", client);
                }
            }

            buffer.on_editor_action();
        }
    }

    fn execute_status_bar_command(&mut self, cmd: &EditorCommand) {

    }

    fn execute_file_tree_command(&mut self, cmd: &EditorCommand) {
        
    }

    fn update_region_type(&mut self) {
        if Self::inside_region(self.mouse_pos, self.layout.buffer_origin, self.layout.buffer_extents) {
            if self.region_type != RegionType::Text {
                unsafe { SendMessageW(self.hwnd, WM_REGION_CHANGED, RegionType::to_usize(RegionType::Text), 0); }
                self.region_type = RegionType::Text;
            }
        }
        else if Self::inside_region(self.mouse_pos, self.layout.status_bar_origin, self.layout.status_bar_extents) {
            if self.region_type != RegionType::Display {
                unsafe { SendMessageW(self.hwnd, WM_REGION_CHANGED, RegionType::to_usize(RegionType::Display), 0); }
                self.region_type = RegionType::Display;
            }
        }
        else if Self::inside_region(self.mouse_pos, self.layout.file_tree_origin, self.layout.file_tree_extents) {
            if self.region_type != RegionType::Display {
                unsafe { SendMessageW(self.hwnd, WM_REGION_CHANGED, RegionType::to_usize(RegionType::Display), 0); }
                self.region_type = RegionType::Display;
            }
        }
    }

    pub fn execute_command(&mut self, cmd: &EditorCommand) {
        match *cmd {
            EditorCommand::MouseMove(mouse_pos) if !self.mouse_pos_captured => {
                self.mouse_pos = mouse_pos;
                self.update_region_type();
            }
            _ => {}
        }

        if Self::inside_region(self.mouse_pos, self.layout.buffer_origin, self.layout.buffer_extents) {
            self.execute_buffer_command(cmd);
        }
        else if Self::inside_region(self.mouse_pos, self.layout.status_bar_origin, self.layout.status_bar_extents) {
            self.execute_status_bar_command(cmd);
        }
        else if Self::inside_region(self.mouse_pos, self.layout.file_tree_origin, self.layout.file_tree_extents) {
            self.execute_file_tree_command(cmd);
        }
    }
}
