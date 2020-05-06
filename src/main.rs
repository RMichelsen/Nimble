#![feature(new_uninit)]
#![feature(const_fn)]
#![feature(clamp)]
#![windows_subsystem = "console"]

mod editor;
mod renderer;
mod theme;
mod buffer;
mod lsp_client;
mod lsp_structs;
mod settings;
mod language_support;
mod text_utils;
mod status_bar;
mod file_tree;

use editor::{ Editor, RegionType, EditorCommand };
use settings::MAX_LSP_RESPONSE_SIZE;

use std::{
    alloc::{dealloc, Layout},
    ffi::OsStr,
    mem::MaybeUninit,
    os::windows::ffi::OsStrExt,
    iter::once,
    ptr::null_mut,
    time::Duration,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering}
    },
    thread
};

use winapi::{
    um::{
        combaseapi::{
            CoInitializeEx,
            CoUninitialize
        },
        winuser::{
            SetWindowLongPtrW, GetWindowLongPtrW,
            UnregisterClassW, DispatchMessageW,
            TranslateMessage, GetMessageW, SendMessageW, 
            ShowWindow, CreateWindowExW, SetCursor,
            SetProcessDpiAwarenessContext, PostQuitMessage,
            DefWindowProcW, RegisterClassW, LoadCursorW, 
            SetCapture, ReleaseCapture,
            BeginPaint, EndPaint, GET_WHEEL_DELTA_WPARAM,
            CW_USEDEFAULT, MSG, IDC_ARROW, GetKeyState,
            WM_PAINT, WM_SIZE, WM_DESTROY, WM_CHAR, IDC_SIZEWE,
            WM_MOUSEWHEEL, WM_LBUTTONDOWN, WM_ERASEBKGND, WM_MOUSELEAVE,
            WM_LBUTTONUP, WM_KEYDOWN, VK_SHIFT, VK_CONTROL,
            WM_CREATE, CREATESTRUCTW, GWLP_USERDATA, IDC_IBEAM,
            WM_MOUSEMOVE, WM_NCDESTROY, SW_SHOW, WM_LBUTTONDBLCLK,
            WS_OVERLAPPEDWINDOW, CS_HREDRAW, CS_VREDRAW, CS_DBLCLKS,
            WNDCLASSW, PAINTSTRUCT, InvalidateRect, DestroyWindow,
            SIZE_MINIMIZED, TRACKMOUSEEVENT, TME_LEAVE, HOVER_DEFAULT,
            TrackMouseEvent
        },
        errhandlingapi::GetLastError,
        wingdi::{GetStockObject, BLACK_BRUSH}
    },
    shared::{
        windef::{
            HWND, HMENU, HBRUSH, HICON, HCURSOR,
            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2
        },
        minwindef::{
            WPARAM, LPARAM, LRESULT, HINSTANCE,
            LOWORD, HIWORD
        },
        ntdef::LPCWSTR,
        windowsx::{GET_X_LPARAM, GET_Y_LPARAM}
    },
    ctypes::c_void
};

const WM_CARET_VISIBLE:     u32 = 0xC000;
const WM_CARET_INVISIBLE:   u32 = 0xC001;
const WM_LSP_RESPONSE:      u32 = 0xC002;
const WM_LSP_CRASH:         u32 = 0xC003; 
const WM_REGION_CHANGED:    u32 = 0xC004;

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let editor: *mut Editor;
    if msg == WM_CREATE {
        let create = lparam as *mut CREATESTRUCTW;
        let uninit_editor = (*create).lpCreateParams as *mut Box<MaybeUninit<Editor>>;

        // Create the editor
        (*uninit_editor).as_mut_ptr().write(Editor::new(hwnd));
        
        // Set the box to be carried over to subsequent callbacks
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, (*uninit_editor).as_mut_ptr() as isize);
        editor = (*uninit_editor).as_mut_ptr();

        (*editor).draw();
    }
    else {
        editor = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Editor;
    }

    let shift_down = (GetKeyState(VK_SHIFT) & 0x80) != 0;
    let ctrl_down = (GetKeyState(VK_CONTROL) & 0x80) != 0;

    static mut MOUSE_FROM_OUTSIDE_WINDOW: bool = false;
    match msg {
        WM_REGION_CHANGED => {
            match RegionType::from_usize(wparam) {
                RegionType::FileTree => { SetCursor(LoadCursorW(null_mut(), IDC_ARROW)); },
                RegionType::TextBuffer => { SetCursor(LoadCursorW(null_mut(), IDC_IBEAM)); },
                RegionType::ResizableBorder => { SetCursor(LoadCursorW(null_mut(), IDC_SIZEWE)); }
                RegionType::StatusBar => { SetCursor(LoadCursorW(null_mut(), IDC_ARROW)); }
                RegionType::Unknown => {}
            }
            InvalidateRect(hwnd, null_mut(), false as i32);
            0
        }
        WM_LSP_RESPONSE => {
            let data = wparam as *mut u8;
            let range = std::mem::transmute::<isize, (i32, i32)>(lparam); 
            let slice: &[u8] = core::slice::from_raw_parts_mut(data.offset(range.0 as isize), range.1 as usize);
            let response = std::str::from_utf8(slice).unwrap();
            (*editor).process_language_server_response(response);
            
            // We have to deallocte the buffer holding the response here,
            // it is NOT done by the LSP thread itself
            dealloc(data, Layout::from_size_align(MAX_LSP_RESPONSE_SIZE, 8).unwrap());
            InvalidateRect(hwnd, null_mut(), false as i32);
            0
        }
        WM_LSP_CRASH => {
            let len = lparam as usize;
            let client = std::str::from_utf8(std::slice::from_raw_parts(wparam as *const u8, len)).unwrap();
            (*editor).execute_command(&EditorCommand::LSPClientCrash(client));
            0
        }
        WM_CARET_VISIBLE => {
            (*editor).execute_command(&EditorCommand::CaretVisible);
            InvalidateRect(hwnd, null_mut(), false as i32);
            0
        }
        WM_CARET_INVISIBLE => {
            (*editor).execute_command(&EditorCommand::CaretInvisible);
            InvalidateRect(hwnd, null_mut(), false as i32);
            0
        }
        WM_PAINT => {
            let mut ps = MaybeUninit::<PAINTSTRUCT>::uninit();
            BeginPaint(hwnd, ps.as_mut_ptr());
            (*editor).draw();
            EndPaint(hwnd, ps.as_mut_ptr());
            0
        }
        WM_ERASEBKGND => {
            0
        }
        WM_SIZE => {
            // If the window is being minimized just return
            if wparam == SIZE_MINIMIZED {
                return 0;
            }
            let width = LOWORD(lparam as u32);
            let height = HIWORD(lparam as u32);
            (*editor).resize(width.into(), height.into());
            InvalidateRect(hwnd, null_mut(), false as i32);
            0
        }
        WM_DESTROY | WM_NCDESTROY => {
            PostQuitMessage(0);
            0
        }
        WM_CHAR => {
            if wparam >= 0x20 && wparam <= 0x7E {
                (*editor).execute_command(&EditorCommand::CharInsert(wparam as u16));
            }
            0
        }
        WM_MOUSEWHEEL => {
            if GET_WHEEL_DELTA_WPARAM(wparam) > 0 {
                (*editor).execute_command(&EditorCommand::ScrollUp(ctrl_down));
            }
            else {
                (*editor).execute_command(&EditorCommand::ScrollDown(ctrl_down));
            }
            InvalidateRect(hwnd, null_mut(), false as i32);
            0
        }
        WM_LBUTTONDOWN => {
            SetCapture(hwnd);
            (*editor).capture_mouse();
            let mouse_pos = (GET_X_LPARAM(lparam) as f32, GET_Y_LPARAM(lparam) as f32);
            (*editor).execute_command(&EditorCommand::LeftClick(mouse_pos, shift_down));
            InvalidateRect(hwnd, null_mut(), false as i32);
            0
        }
        WM_LBUTTONDBLCLK => {
            let mouse_pos = (GET_X_LPARAM(lparam) as f32, GET_Y_LPARAM(lparam) as f32);
            (*editor).execute_command(&EditorCommand::LeftDoubleClick(mouse_pos));
            InvalidateRect(hwnd, null_mut(), false as i32);
            0
        }
        WM_LBUTTONUP => {
            ReleaseCapture();
            (*editor).release_mouse();
            (*editor).execute_command(&EditorCommand::LeftRelease);
            InvalidateRect(hwnd, null_mut(), false as i32);
            0
        }
        WM_KEYDOWN => {
            (*editor).execute_command(&EditorCommand::KeyPressed(wparam as i32, shift_down, ctrl_down));
            InvalidateRect(hwnd, null_mut(), false as i32);
            0
        }
        WM_MOUSEMOVE => {
            // If the mouse came from outside the window,
            // track when the mouse leaves the window (and fires the WM_MOUSELEAVE event)
            if MOUSE_FROM_OUTSIDE_WINDOW {
                let mut mouse_tracker = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: HOVER_DEFAULT
                };
                TrackMouseEvent(&mut mouse_tracker as *mut _);
                MOUSE_FROM_OUTSIDE_WINDOW = false;
            }

            let mouse_pos = (GET_X_LPARAM(lparam) as f32, GET_Y_LPARAM(lparam) as f32);
            (*editor).execute_command(&EditorCommand::MouseMove(mouse_pos));
            if (*editor).selection_active() {
                InvalidateRect(hwnd, null_mut(), false as i32);
            }
            0
        }
        WM_MOUSELEAVE => {
            (*editor).mouse_left_window();
            MOUSE_FROM_OUTSIDE_WINDOW = true;
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam)
    }
}

fn main() {
    let mut editor = Box::<Editor>::new_uninit();

    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        // Start up the COM library, necessary for file dialogs
        // 2 | 4 = COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE
        CoInitializeEx(null_mut(), 2 | 4);

        let wnd_name: Vec<u16> = OsStr::new("Keen").encode_wide().chain(once(0)).collect();
        let wnd_class_name: Vec<u16> = OsStr::new("Keen_Class").encode_wide().chain(once(0)).collect();

        let wnd_class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS,
            lpfnWndProc: Some(wnd_proc),
            lpszClassName: wnd_class_name.as_ptr(),
            lpszMenuName: 0 as LPCWSTR,
            hInstance: 0 as HINSTANCE,
            hIcon: 0 as HICON,
            hCursor: 0 as HCURSOR,
            hbrBackground: GetStockObject(BLACK_BRUSH as i32) as HBRUSH,
            cbClsExtra: 0,
            cbWndExtra: 0
        };

        let hr = RegisterClassW(&wnd_class);
        assert!(hr != 0, "Failed to register the window class, win32 error code: {}", hr);

        let hwnd = CreateWindowExW(
            0,
            wnd_class_name.as_ptr(),
            wnd_name.as_ptr(),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            0 as HWND,
            0 as HMENU,
            0 as HINSTANCE,
            (&mut editor as *mut _) as *mut c_void
        );
        assert!(hwnd != (0 as HWND), "Failed to open window, win32 error code: {}", GetLastError());
        ShowWindow(hwnd, SW_SHOW);

        let mut mouse_tracker = TRACKMOUSEEVENT {
            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE,
            hwndTrack: hwnd,
            dwHoverTime: HOVER_DEFAULT
        };
        TrackMouseEvent(&mut mouse_tracker as *mut _);

        let mut msg = MaybeUninit::<MSG>::uninit();

        let force_finish = Arc::new(AtomicBool::new(false));
        let force_finish_clone = force_finish.clone();
        let hwnd_clone = hwnd as u64;
        let caret_blink_thread = thread::spawn(move|| {
            let mut message_idx: u32 = 0;
            
            // To be able to responsively exit the thread on
            // program close we will poll the thread every 50ms
            let mut counter = 1;
            while !force_finish_clone.load(Ordering::Relaxed) {
                if counter > 10 {
                    SendMessageW(hwnd_clone as HWND, WM_CARET_VISIBLE + message_idx, 0, 0);
                    message_idx ^= 1;
                    counter = 1;
                }

                thread::sleep(Duration::from_millis(50));
                counter += 1;
            }
        });

        while GetMessageW(msg.as_mut_ptr(), 0 as HWND, 0, 0) > 0 {
            TranslateMessage(msg.as_mut_ptr());
            DispatchMessageW(msg.as_mut_ptr());
        }

        force_finish.store(true, Ordering::Relaxed);
        if let Err(panic) = caret_blink_thread.join() {
            println!("Caret blink thread failed with error: {:?}", panic);
        }

        CoUninitialize();
        UnregisterClassW(wnd_class_name.as_ptr(), 0 as HINSTANCE);
        DestroyWindow(hwnd);
    }
}
