#![cfg(target_os = "windows")]
#![allow(clippy::fn_to_numeric_cast)]

use tauri::WebviewWindow;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, GetWindowRect, PostMessageW, SetWindowLongPtrW, GWLP_WNDPROC, HTBOTTOM,
    HTBOTTOMLEFT, HTBOTTOMRIGHT, HTCLIENT, HTLEFT, HTMAXBUTTON, HTRIGHT, HTTOP, HTTOPLEFT,
    HTTOPRIGHT, SC_MAXIMIZE, SC_RESTORE, WM_NCHITTEST, WM_NCLBUTTONUP, WM_SYSCOMMAND,
};

const TITLEBAR_H: i32 = 44;
const RESIZE_EDGE: i32 = 6;
const CTRL_W: i32 = 44;

static mut ORIGINAL_PROC: isize = 0;

pub fn install(window: &WebviewWindow) {
    let Ok(hwnd) = window.hwnd() else {
        tracing::warn!("no HWND; skipping frameless subclass");
        return;
    };
    let hwnd = HWND(hwnd.0 as _);
    let proc: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT = wndproc;
    unsafe {
        let prev = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, proc as isize);
        ORIGINAL_PROC = prev;
    }
    tracing::info!("frameless subclass installed (resize + snap layouts)");
}

fn scale(hwnd: HWND) -> f32 {
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    if dpi == 0 {
        1.0
    } else {
        dpi as f32 / 96.0
    }
}

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let chain = |h, m, w, l| {
        let orig = ORIGINAL_PROC;
        if orig == 0 {
            DefWindowProcW(h, m, w, l)
        } else {
            let f: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT =
                std::mem::transmute(orig);
            f(h, m, w, l)
        }
    };

    if crate::thumbbar::handle_message(hwnd, msg, wparam.0) {
        return LRESULT(0);
    }

    match msg {
        WM_NCHITTEST => {
            let ht = hit_test(hwnd, lparam);
            if ht == HTCLIENT as i32 {
                chain(hwnd, msg, wparam, lparam)
            } else {
                LRESULT(ht as isize)
            }
        }
        WM_NCLBUTTONUP if wparam.0 as i32 == HTMAXBUTTON as i32 => {
            let maximized = is_maximized(hwnd);
            let cmd = if maximized { SC_RESTORE } else { SC_MAXIMIZE };
            let _ = PostMessageW(Some(hwnd), WM_SYSCOMMAND, WPARAM(cmd as usize), LPARAM(0));
            LRESULT(0)
        }
        _ => chain(hwnd, msg, wparam, lparam),
    }
}

fn hit_test(hwnd: HWND, lparam: LPARAM) -> i32 {
    let mut rect = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut rect) }.is_err() {
        return HTCLIENT as i32;
    }

    let sx = (lparam.0 & 0xFFFF) as i16 as i32;
    let sy = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;

    let s = scale(hwnd);
    let edge = (RESIZE_EDGE as f32 * s) as i32;
    let bar = (TITLEBAR_H as f32 * s) as i32;
    let ctrl = (CTRL_W as f32 * s) as i32;

    let left = sx < rect.left + edge;
    let right = sx >= rect.right - edge;
    let top = sy < rect.top + edge;
    let bottom = sy >= rect.bottom - edge;

    if !is_maximized(hwnd) {
        match (top, bottom, left, right) {
            (true, _, true, _) => return HTTOPLEFT as i32,
            (true, _, _, true) => return HTTOPRIGHT as i32,
            (_, true, true, _) => return HTBOTTOMLEFT as i32,
            (_, true, _, true) => return HTBOTTOMRIGHT as i32,
            (true, _, _, _) => return HTTOP as i32,
            (_, true, _, _) => return HTBOTTOM as i32,
            (_, _, true, _) => return HTLEFT as i32,
            (_, _, _, true) => return HTRIGHT as i32,
            _ => {}
        }
    }

    let in_titlebar = sy < rect.top + bar;
    let max_r = rect.right - ctrl; // close occupies the last CTRL_W
    let max_l = rect.right - ctrl * 2;
    if in_titlebar && sx >= max_l && sx < max_r {
        return HTMAXBUTTON as i32;
    }

    HTCLIENT as i32
}

fn is_maximized(hwnd: HWND) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowPlacement, SW_SHOWMAXIMIZED, WINDOWPLACEMENT};
    let mut wp = WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
    if unsafe { GetWindowPlacement(hwnd, &mut wp) }.is_ok() {
        wp.showCmd == SW_SHOWMAXIMIZED.0 as u32
    } else {
        false
    }
}
