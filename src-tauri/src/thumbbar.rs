use std::cell::RefCell;
use std::sync::OnceLock;

use tauri::{AppHandle, Manager, WebviewWindow};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::{
    ITaskbarList3, TaskbarList, THUMBBUTTON, THUMBBUTTONFLAGS, THUMBBUTTONMASK,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIconFromResourceEx, GetSystemMetrics, LookupIconIdFromDirectoryEx, RegisterWindowMessageW,
    HICON, IMAGE_FLAGS, SM_CXSMICON, WM_COMMAND,
};
use windows::core::w;

use crate::player::Status;

const ID_PREV: u32 = 1;
const ID_PLAY_PAUSE: u32 = 2;
const ID_NEXT: u32 = 3;

const THBN_CLICKED: u16 = 0x1800;

const ICON_PREV: &[u8] = include_bytes!("../icons/thumb/prev.ico");
const ICON_PLAY: &[u8] = include_bytes!("../icons/thumb/play.ico");
const ICON_PAUSE: &[u8] = include_bytes!("../icons/thumb/pause.ico");
const ICON_NEXT: &[u8] = include_bytes!("../icons/thumb/next.ico");

static BUTTON_CREATED: OnceLock<u32> = OnceLock::new();
static APP: OnceLock<AppHandle> = OnceLock::new();

thread_local! {
    static TASKBAR: RefCell<Option<ITaskbarList3>> = const { RefCell::new(None) };
    static ADDED: RefCell<bool> = const { RefCell::new(false) };
}

fn play_pause(status: Status) -> (&'static [u8], &'static str) {
    if matches!(status, Status::Playing) {
        (ICON_PAUSE, "Pause")
    } else {
        (ICON_PLAY, "Play")
    }
}

pub fn install(window: &WebviewWindow, app: &AppHandle) {
    let Ok(hwnd) = window.hwnd() else {
        tracing::warn!("no HWND; skipping thumbbar");
        return;
    };
    let id = unsafe { RegisterWindowMessageW(w!("TaskbarButtonCreated")) };
    if id == 0 {
        tracing::warn!("could not register TaskbarButtonCreated; skipping thumbbar");
        return;
    }
    let _ = BUTTON_CREATED.set(id);
    let _ = APP.set(app.clone());

    ensure_added(hwnd);
    tracing::info!("thumbbar armed");
}

fn ensure_added(hwnd: HWND) {
    if ADDED.with(|a| *a.borrow()) {
        return;
    }
    add_buttons(hwnd);
}

pub fn handle_message(hwnd: HWND, msg: u32, wparam: usize) -> bool {
    if Some(&msg) == BUTTON_CREATED.get() {
        ADDED.with(|a| *a.borrow_mut() = false);
        add_buttons(hwnd);
        return true;
    }
    if msg == WM_COMMAND {
        if let Some(id) = clicked_button(wparam) {
            if let Some(app) = APP.get() {
                on_click(app, id);
            }
            return true;
        }
    }
    false
}

pub fn clicked_button(wparam: usize) -> Option<u32> {
    let high = ((wparam >> 16) & 0xffff) as u16;
    if high != THBN_CLICKED {
        return None;
    }
    Some((wparam & 0xffff) as u32)
}

fn icon_size() -> i32 {
    let n = unsafe { GetSystemMetrics(SM_CXSMICON) };
    if n <= 0 {
        16
    } else {
        n
    }
}

fn load_icon(bytes: &[u8], size: i32) -> Option<HICON> {
    unsafe {
        let offset = LookupIconIdFromDirectoryEx(bytes.as_ptr(), true, size, size, IMAGE_FLAGS(0));
        if offset == 0 {
            return None;
        }
        let res = bytes.get(offset as usize..)?;
        CreateIconFromResourceEx(res, true, 0x0003_0000, size, size, IMAGE_FLAGS(0)).ok()
    }
}

fn buttons(status: Status) -> Vec<THUMBBUTTON> {
    let size = icon_size();
    let (mid_icon, mid_tip) = play_pause(status);

    [(ID_PREV, ICON_PREV, "Previous"), (ID_PLAY_PAUSE, mid_icon, mid_tip), (ID_NEXT, ICON_NEXT, "Next")]
        .into_iter()
        .map(|(id, bytes, tip)| {
            let mut b = THUMBBUTTON {
                dwMask: THUMBBUTTONMASK(0x2 | 0x4 | 0x8), // ICON | TOOLTIP | FLAGS
                iId: id,
                dwFlags: THUMBBUTTONFLAGS(0), // enabled
                ..Default::default()
            };
            if let Some(icon) = load_icon(bytes, size) {
                b.hIcon = icon;
            }
            for (i, c) in tip.encode_utf16().take(b.szTip.len() - 1).enumerate() {
                b.szTip[i] = c;
            }
            b
        })
        .collect()
}

fn taskbar() -> Option<ITaskbarList3> {
    TASKBAR.with(|slot| {
        if let Some(existing) = slot.borrow().clone() {
            return Some(existing);
        }
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let list: ITaskbarList3 = match CoCreateInstance(&TaskbarList, None, CLSCTX_ALL) {
                Ok(l) => l,
                Err(e) => {
                    tracing::warn!(error = %e, "no ITaskbarList3; thumbbar unavailable");
                    return None;
                }
            };
            if let Err(e) = list.HrInit() {
                tracing::warn!(error = %e, "ITaskbarList3 HrInit failed");
                return None;
            }
            *slot.borrow_mut() = Some(list.clone());
            Some(list)
        }
    })
}

fn add_buttons(hwnd: HWND) {
    let Some(list) = taskbar() else { return };
    let btns = buttons(Status::Idle);
    match unsafe { list.ThumbBarAddButtons(hwnd, &btns) } {
        Ok(()) => {
            ADDED.with(|a| *a.borrow_mut() = true);
            tracing::info!("thumbbar buttons added");
        }
        Err(e) => tracing::warn!(error = %e, "could not add thumbbar buttons"),
    }
}

pub fn refresh(app: &AppHandle, status: Status) {
    let handle = app.clone();
    let _ = app.clone().run_on_main_thread(move || {
        let Some(window) = handle.get_webview_window("main") else { return };
        let Ok(hwnd) = window.hwnd() else { return };

        ensure_added(hwnd);
        if !ADDED.with(|a| *a.borrow()) {
            return;
        }

        let Some(list) = taskbar() else { return };
        let btns = buttons(status);
        if let Err(e) = unsafe { list.ThumbBarUpdateButtons(hwnd, &btns) } {
            tracing::debug!(error = %e, "thumbbar update failed");
        }
    });
}

pub fn on_click(app: &AppHandle, id: u32) {
    match id {
        ID_PREV => crate::commands::apply(app, |p| p.previous_track()),
        ID_PLAY_PAUSE => crate::commands::apply(app, |p| p.toggle()),
        ID_NEXT => crate::commands::apply(app, |p| p.next_track()),
        other => tracing::debug!(id = other, "unknown thumbbar button"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn middle_button_shows_pause_only_while_playing() {
        let (icon, tip) = play_pause(Status::Playing);
        assert_eq!(tip, "Pause");
        assert_eq!(icon, ICON_PAUSE);

        for s in [Status::Idle, Status::Paused, Status::Loading, Status::Stalled, Status::Ended] {
            let (icon, tip) = play_pause(s);
            assert_eq!(tip, "Play", "{s:?} must offer play");
            assert_eq!(icon, ICON_PLAY, "{s:?} must show the play glyph");
        }

        assert_ne!(ICON_PLAY, ICON_PAUSE);
    }

    #[test]
    fn only_thumbbar_commands_are_claimed() {
        let wparam = ((THBN_CLICKED as usize) << 16) | ID_NEXT as usize;
        assert_eq!(clicked_button(wparam), Some(ID_NEXT));

        assert_eq!(clicked_button(ID_NEXT as usize), None);
    }
}
