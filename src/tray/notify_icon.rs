//! The notification-area icon: adding/updating it, its tooltip, and
//! notifications (balloons).

use std::ffi::c_void;

use windows::Win32::Graphics::Gdi::{CreateBitmap, CreateDIBSection, DeleteObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIIF_NOSOUND, NIIF_WARNING, NIM_ADD,
    NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, LoadIconW, HICON, ICONINFO, IDI_APPLICATION};

use super::{App, WM_TRAY};
use crate::capture::DiscordStatus;

const TRAY_UID: u32 = 1;

pub(super) fn copy_wide(dst: &mut [u16], s: &str) {
    let mut n = 0;
    let max = dst.len() - 1;
    for (d, c) in dst.iter_mut().zip(s.encode_utf16()).take(max) {
        *d = c;
        n += 1;
    }
    dst[n] = 0;
}

pub(super) fn base_nid(app: &App) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: app.hwnd.get(),
        uID: TRAY_UID,
        ..Default::default()
    }
}

pub(super) fn discord_line(app: &App) -> (String, bool) {
    match &*app.discord_status.lock().unwrap() {
        DiscordStatus::Waiting => ("Discord: not running (waiting)".into(), false),
        DiscordStatus::Capturing { pid, exe } => (format!("Discord: capturing {exe} (pid {pid})"), true),
        DiscordStatus::Failed(e) => (format!("Discord: error - {e}"), false),
    }
}

pub(super) fn tray_nid(app: &App) -> NOTIFYICONDATAW {
    let (_, active) = discord_line(app);
    let cfg = app.cfg.borrow();
    let mut nid = base_nid(app);
    nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    nid.uCallbackMessage = WM_TRAY;
    nid.hIcon = if active { app.icon_active.get() } else { app.icon_idle.get() };
    let short = if active { "Discord: capturing" } else { "Discord: waiting" };
    copy_wide(
        &mut nid.szTip,
        &format!("Discord Clip\n{} saves the last {}s\n{short}", cfg.hotkey, app.running_seconds),
    );
    nid
}

pub(super) fn tray_add(app: &App) {
    unsafe {
        let _ = Shell_NotifyIconW(NIM_ADD, &tray_nid(app));
    }
}

pub(super) fn tray_update(app: &App) {
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &tray_nid(app));
    }
}

pub(super) fn tray_remove(app: &App) {
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &base_nid(app));
    }
}

/// Shows a tray notification. Warnings are shown even when the user turned
/// notifications off. Silent: we beep ourselves (and Windows mutes
/// notification sounds while gaming anyway).
pub(super) fn notify(app: &App, title: &str, text: &str, warning: bool) {
    if !warning && !app.cfg.borrow().notifications {
        return;
    }
    let mut nid = base_nid(app);
    nid.uFlags = NIF_INFO;
    copy_wide(&mut nid.szInfoTitle, title);
    copy_wide(&mut nid.szInfo, text);
    nid.dwInfoFlags = if warning { NIIF_WARNING } else { NIIF_INFO } | NIIF_NOSOUND;
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

/// An info notification that ignores the "show notifications" setting
/// (that one is about saved clips). For rare things like updates.
pub(super) fn notify_always(app: &App, title: &str, text: &str) {
    let mut nid = base_nid(app);
    nid.uFlags = NIF_INFO;
    copy_wide(&mut nid.szInfoTitle, title);
    copy_wide(&mut nid.szInfo, text);
    nid.dwInfoFlags = NIIF_INFO | NIIF_NOSOUND;
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}


pub(super) fn make_icon(size: i32, rgb: (u8, u8, u8)) -> HICON {
    let px = crate::icon::bgra(size, rgb);
    unsafe {
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size,
                biHeight: -size, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let Ok(color) = CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) else {
            return LoadIconW(None, IDI_APPLICATION).unwrap_or_default();
        };
        std::ptr::copy_nonoverlapping(px.as_ptr(), bits as *mut u8, px.len());
        let mask_bits = vec![0u8; ((size + 15) / 16 * 2 * size) as usize];
        let mask = CreateBitmap(size, size, 1, 1, Some(mask_bits.as_ptr() as *const c_void));
        let info = ICONINFO {
            fIcon: true.into(),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let icon = CreateIconIndirect(&info).unwrap_or_else(|_| LoadIconW(None, IDI_APPLICATION).unwrap_or_default());
        let _ = DeleteObject(color);
        let _ = DeleteObject(mask);
        icon
    }
}

