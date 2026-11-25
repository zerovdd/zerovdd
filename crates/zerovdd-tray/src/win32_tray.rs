//! Native Win32 system tray window, menu dispatch, and action handlers for `zerovdd-tray`.

use core::mem::size_of;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{bail, Context, Result};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, GlobalFree, ERROR_ALREADY_EXISTS, ERROR_SUCCESS, HANDLE, HINSTANCE,
    HWND, LPARAM, LRESULT, POINT, WPARAM,
};
use windows::Win32::Graphics::Gdi::HBRUSH;
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
    HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ,
};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Shell::{
    ShellExecuteW, Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
    NIM_SETVERSION, NIN_SELECT, NOTIFYICONDATAW, NOTIFYICONDATAW_0, NOTIFYICON_VERSION_4,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, GetCursorPos, GetMessageW, GetWindowLongPtrW, KillTimer, LoadIconW,
    MessageBoxW, PostMessageW, PostQuitMessage, RegisterClassExW, RegisterWindowMessageW,
    SetForegroundWindow, SetTimer, SetWindowLongPtrW, TrackPopupMenuEx, TranslateMessage,
    CW_USEDEFAULT, GWLP_USERDATA, HICON, HMENU, IDI_APPLICATION, MB_ICONERROR, MB_OK, MF_CHECKED,
    MF_ENABLED, MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING, MF_UNCHECKED, MSG, SW_SHOWNORMAL,
    TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_APP, WM_CONTEXTMENU, WM_DESTROY, WM_LBUTTONUP,
    WM_NULL, WM_RBUTTONUP, WM_TIMER, WNDCLASSEXW, WS_EX_TOOLWINDOW, WS_OVERLAPPEDWINDOW,
};
use zerovdd_config::{default_config_path, save_atomic, ConfigV1};
use zerovdd_proto::ipc::{
    DisplayRemoveRequest, DisplaySetModeRequest, IpcRequest, StopSessionRequest, SwitchGpuRequest,
};

use zerovdd_tray::ipc_client;
use zerovdd_tray::model::{build_tray_menu, MenuAction, MenuNode};
use zerovdd_tray::presets::QUICK_ADD_PRESETS;

const SINGLE_INSTANCE_MUTEX_NAME: &str = "Local\\ZeroVddTraySingleInstance";
const TRAY_WINDOW_CLASS: &str = "ZeroVddTrayWindowClass";
const TRAY_TOOLTIP: &str = "ZeroVDD Virtual Display Driver";
const RUN_REG_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const RUN_VALUE_NAME: &str = "ZeroVDD";
const WM_TRAY_CALLBACK: u32 = WM_APP + 1;
const TRAY_ICON_ID: u32 = 1;
const MENU_CMD_BASE: u32 = 1000;
const CF_UNICODETEXT_FORMAT: u32 = 13;
const CREATE_NO_WINDOW_FLAG: u32 = 0x0800_0000;
const NIN_KEYSELECT_MSG: u32 = NIN_SELECT + 1;
const TRAY_RETRY_TIMER_ID: usize = 1;

struct MutexGuard(HANDLE);

impl Drop for MutexGuard {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: `self.0` was returned by `CreateMutexW` and is closed once on drop.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

struct TrayState {
    icon: HICON,
    taskbar_created_msg: u32,
    debug: bool,
}

fn to_wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(core::iter::once(0)).collect()
}

/// Starts the native Win32 hidden-window tray message loop until Exit or WM_DESTROY.
pub fn run_tray_loop(debug: bool) -> Result<()> {
    let mutex_name = to_wide(SINGLE_INSTANCE_MUTEX_NAME);
    // SAFETY: `mutex_name` is NUL-terminated UTF-16 naming a session-local mutex.
    let mutex_handle = unsafe { CreateMutexW(None, false, PCWSTR(mutex_name.as_ptr())) }
        .context("CreateMutexW failed for ZeroVDD tray single-instance mutex")?;
    let _mutex_guard = MutexGuard(mutex_handle);

    // SAFETY: Calling `GetLastError` immediately after `CreateMutexW` checks whether an earlier
    // instance already holds `SINGLE_INSTANCE_MUTEX_NAME`.
    let already_running = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    if already_running {
        if debug {
            eprintln!("zerovdd-tray is already running; exiting duplicate instance cleanly.");
        }
        return Ok(());
    }

    // SAFETY: `GetModuleHandleW(None)` retrieves the current executable's module handle.
    let module = unsafe { GetModuleHandleW(None) }.context("GetModuleHandleW failed")?;
    let hinstance = HINSTANCE(module.0);

    // WHY: Resource ID `1` is embedded by `build.rs` from `assets/zerovdd.rc`; fall back to
    // `IDI_APPLICATION` if compiled without `rc.exe`/`llvm-rc.exe`.
    // SAFETY: `hinstance` is valid for this module and resource ID `1` is embedded by `build.rs`.
    let icon = unsafe {
        LoadIconW(
            Some(hinstance),
            PCWSTR(core::ptr::without_provenance::<u16>(TRAY_ICON_ID as usize)),
        )
    }
    .or_else(|_| {
        // SAFETY: `IDI_APPLICATION` is a built-in system icon resource independent of `hinstance`.
        unsafe { LoadIconW(None, IDI_APPLICATION) }
    })
    .context("failed to load tray icon resource or fallback IDI_APPLICATION")?;

    let class_name = to_wide(TRAY_WINDOW_CLASS);
    let wc = WNDCLASSEXW {
        cbSize: u32::try_from(size_of::<WNDCLASSEXW>()).unwrap_or(0),
        lpfnWndProc: Some(tray_wnd_proc),
        hInstance: hinstance,
        lpszClassName: PCWSTR(class_name.as_ptr()),
        hbrBackground: HBRUSH::default(),
        ..Default::default()
    };

    // SAFETY: `wc` points to a valid static `tray_wnd_proc` and NUL-terminated `class_name`.
    let atom = unsafe { RegisterClassExW(&wc) };
    if atom == 0 {
        bail!("RegisterClassExW failed for `{TRAY_WINDOW_CLASS}`");
    }

    let taskbar_msg_name = to_wide("TaskbarCreated");
    // SAFETY: `taskbar_msg_name` is a NUL-terminated UTF-16 string.
    let taskbar_created_msg = unsafe { RegisterWindowMessageW(PCWSTR(taskbar_msg_name.as_ptr())) };

    let window_title = to_wide("ZeroVDD Tray");
    // WHY: Use a hidden top-level window rather than `HWND_MESSAGE` so `WM_TASKBARCREATED`
    // broadcasts from Explorer reach `tray_wnd_proc` after taskbar restarts.
    // SAFETY: `class_name`, `window_title`, and `hinstance` remain valid across creation.
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            PCWSTR(class_name.as_ptr()),
            PCWSTR(window_title.as_ptr()),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            None,
            None,
            Some(hinstance),
            None,
        )
    }
    .context("CreateWindowExW failed for ZeroVDD tray window")?;

    let state = Box::new(TrayState {
        icon,
        taskbar_created_msg,
        debug,
    });
    let state_ptr = Box::into_raw(state);
    // SAFETY: `hwnd` was just created on this thread and `state_ptr` is owned until `WM_DESTROY`.
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr as isize);
    }

    if !add_tray_icon(hwnd, icon) {
        if debug {
            eprintln!(
                "Shell_NotifyIconW(NIM_ADD) deferred; waiting for TaskbarCreated/timer retry."
            );
        }
        // WHY: Retry periodically until Explorer's shell tray window becomes available (e.g.
        // early login or locked desktop), then stop the timer once the icon attaches.
        // SAFETY: `hwnd` is valid on this thread and `TRAY_RETRY_TIMER_ID` is non-zero.
        unsafe {
            let _ = SetTimer(Some(hwnd), TRAY_RETRY_TIMER_ID, 2000, None);
        }
    }

    let mut msg = MSG::default();
    loop {
        // SAFETY: `msg` points to writable stack storage on the message-loop thread.
        let status = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if status.0 <= 0 {
            break;
        }
        // SAFETY: `msg` was just retrieved from `GetMessageW`.
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    Ok(())
}

// SAFETY: Must be called only by Windows message dispatch with a valid window handle `hwnd`.
unsafe extern "system" fn tray_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: `GWLP_USERDATA` holds either null or a live `Box<TrayState>` pointer until `WM_DESTROY`.
    let state_ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut TrayState;

    if msg == WM_DESTROY {
        // SAFETY: Stop any pending tray attachment timer cleanly on teardown.
        unsafe {
            let _ = KillTimer(Some(hwnd), TRAY_RETRY_TIMER_ID);
        }
        remove_tray_icon(hwnd);
        if !state_ptr.is_null() {
            // SAFETY: Reclaims `TrayState` stored in `GWLP_USERDATA` exactly once on destroy.
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                drop(Box::from_raw(state_ptr));
            }
        }
        // SAFETY: Terminates the thread's `GetMessageW` loop cleanly.
        unsafe {
            PostQuitMessage(0);
        }
        return LRESULT(0);
    }

    if !state_ptr.is_null() {
        // SAFETY: `state_ptr` is non-null and alive on the UI thread until `WM_DESTROY`.
        let state = unsafe { &*state_ptr };
        if msg == WM_TIMER && wparam.0 == TRAY_RETRY_TIMER_ID {
            if add_tray_icon(hwnd, state.icon) {
                // SAFETY: Icon attached cleanly; cancel retry timer so idle CPU remains zero.
                unsafe {
                    let _ = KillTimer(Some(hwnd), TRAY_RETRY_TIMER_ID);
                }
            }
            return LRESULT(0);
        }
        if state.taskbar_created_msg != 0 && msg == state.taskbar_created_msg {
            if add_tray_icon(hwnd, state.icon) {
                // SAFETY: Icon attached cleanly on `TaskbarCreated`; cancel retry timer.
                unsafe {
                    let _ = KillTimer(Some(hwnd), TRAY_RETRY_TIMER_ID);
                }
            }
            return LRESULT(0);
        }

        if msg == WM_TRAY_CALLBACK {
            let event = u32::try_from(lparam.0 & 0xFFFF).unwrap_or(0);
            if matches!(
                event,
                WM_RBUTTONUP | WM_LBUTTONUP | WM_CONTEXTMENU | NIN_SELECT | NIN_KEYSELECT_MSG
            ) {
                if let Err(err) = show_tray_menu(hwnd, state.debug) {
                    if state.debug {
                        eprintln!("zerovdd-tray menu action error: {err:#}");
                    }
                    show_error_dialog(hwnd, &format!("{err:#}"));
                }
                return LRESULT(0);
            }
        }
    }

    // SAFETY: Forwards unhandled messages to default Win32 processing.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn notify_icon_data(hwnd: HWND, icon: HICON) -> NOTIFYICONDATAW {
    let mut tip = [0u16; 128];
    for (dst, src) in tip.iter_mut().zip(TRAY_TOOLTIP.encode_utf16()) {
        *dst = src;
    }

    NOTIFYICONDATAW {
        cbSize: u32::try_from(size_of::<NOTIFYICONDATAW>()).unwrap_or(0),
        hWnd: hwnd,
        uID: TRAY_ICON_ID,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: WM_TRAY_CALLBACK,
        hIcon: icon,
        szTip: tip,
        Anonymous: NOTIFYICONDATAW_0 {
            uVersion: NOTIFYICON_VERSION_4,
        },
        ..Default::default()
    }
}

fn add_tray_icon(hwnd: HWND, icon: HICON) -> bool {
    let nid = notify_icon_data(hwnd, icon);
    // SAFETY: `nid` points to a fully initialized `NOTIFYICONDATAW` bound to live `hwnd`.
    let added = unsafe { Shell_NotifyIconW(NIM_ADD, &nid) }.as_bool();
    if added {
        // SAFETY: `NOTIFYICON_VERSION_4` upgrades callback event packing on the registered icon.
        unsafe {
            let _ = Shell_NotifyIconW(NIM_SETVERSION, &nid);
        }
    }
    added
}

fn remove_tray_icon(hwnd: HWND) {
    let nid = notify_icon_data(hwnd, HICON::default());
    // SAFETY: Removes `TRAY_ICON_ID` owned by `hwnd` from the Windows notification area.
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    }
}

fn show_tray_menu(hwnd: HWND, debug: bool) -> Result<()> {
    let snapshot = ipc_client::fetch_snapshot();
    let autostart = is_autostart_enabled();
    let nodes = build_tray_menu(&snapshot, autostart);

    // SAFETY: Creates a fresh popup menu destroyed immediately after `TrackPopupMenuEx`.
    let root_menu = unsafe { CreatePopupMenu() }.context("CreatePopupMenu failed")?;
    let mut actions = Vec::new();
    let mut next_id = MENU_CMD_BASE;

    if let Err(err) = populate_hmenu(root_menu, &nodes, &mut next_id, &mut actions) {
        // SAFETY: Ensures `root_menu` is destroyed on construction failure.
        unsafe {
            let _ = DestroyMenu(root_menu);
        }
        return Err(err);
    }

    let mut pt = POINT::default();
    // SAFETY: `pt` is writable stack storage receiving screen cursor coordinates.
    unsafe {
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(hwnd);
    }

    let flags = (TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON).0;
    // SAFETY: `root_menu` and `hwnd` are valid; `TrackPopupMenuEx` runs synchronously cleanly.
    let selected = unsafe { TrackPopupMenuEx(root_menu, flags, pt.x, pt.y, hwnd, None) };
    // SAFETY: Classic Win32 fix ensuring consecutive tray clicks dismiss cleanly.
    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(root_menu);
    }

    let cmd_id = u32::try_from(selected.0).unwrap_or(0);
    if cmd_id == 0 {
        return Ok(());
    }

    if let Some((_, action)) = actions.into_iter().find(|(id, _)| *id == cmd_id) {
        if debug {
            eprintln!("zerovdd-tray executing menu action: {action:?}");
        }
        execute_menu_action(hwnd, action)?;
    }

    Ok(())
}

fn populate_hmenu(
    menu: HMENU,
    nodes: &[MenuNode],
    next_id: &mut u32,
    actions: &mut Vec<(u32, MenuAction)>,
) -> Result<()> {
    for node in nodes {
        match node {
            MenuNode::Separator => {
                // SAFETY: `menu` is a live `HMENU`; `MF_SEPARATOR` ignores string pointer.
                unsafe { AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null()) }
                    .context("AppendMenuW separator failed")?;
            }
            MenuNode::Item {
                label,
                enabled,
                checked,
                action,
            } => {
                let id = *next_id;
                *next_id = next_id.saturating_add(1);
                let mut flags = MF_STRING;
                flags |= if *enabled { MF_ENABLED } else { MF_GRAYED };
                flags |= if *checked { MF_CHECKED } else { MF_UNCHECKED };
                let wide = to_wide(label);
                // SAFETY: `menu` is a live `HMENU` and `wide` is NUL-terminated UTF-16.
                unsafe { AppendMenuW(menu, flags, id as usize, PCWSTR(wide.as_ptr())) }
                    .with_context(|| format!("AppendMenuW item failed for `{label}`"))?;
                if let Some(act) = action {
                    actions.push((id, act.clone()));
                }
            }
            MenuNode::Submenu {
                label,
                enabled,
                children,
            } => {
                // SAFETY: Child `HMENU` ownership transfers to `menu` when appended with `MF_POPUP`.
                let sub = unsafe { CreatePopupMenu() }.context("CreatePopupMenu child failed")?;
                if let Err(err) = populate_hmenu(sub, children, next_id, actions) {
                    // SAFETY: Destroy child `HMENU` if recursive population fails before attachment.
                    unsafe {
                        let _ = DestroyMenu(sub);
                    }
                    return Err(err);
                }
                let flags = MF_POPUP | if *enabled { MF_ENABLED } else { MF_GRAYED };
                let wide = to_wide(label);
                // SAFETY: `menu` and `sub` are valid `HMENU` handles; `wide` is NUL-terminated.
                if let Err(err) =
                    unsafe { AppendMenuW(menu, flags, sub.0 as usize, PCWSTR(wide.as_ptr())) }
                {
                    // SAFETY: Free unattached child menu on append error.
                    unsafe {
                        let _ = DestroyMenu(sub);
                    }
                    return Err(err)
                        .with_context(|| format!("AppendMenuW submenu failed for `{label}`"));
                }
            }
        }
    }
    Ok(())
}

fn execute_menu_action(hwnd: HWND, action: MenuAction) -> Result<()> {
    match action {
        MenuAction::QuickAddPreset(idx) => {
            let preset = QUICK_ADD_PRESETS
                .get(idx)
                .with_context(|| format!("invalid quick-add preset index {idx}"))?;
            let _ = ipc_client::send_ipc_checked(&IpcRequest::DisplayAdd(preset.to_ipc_request()))?;
        }
        MenuAction::RemoveDisplay { target_id } => {
            let _ =
                ipc_client::send_ipc_checked(&IpcRequest::DisplayRemove(DisplayRemoveRequest {
                    client_id: None,
                    target_id: Some(target_id),
                }))?;
        }
        MenuAction::ToggleDisplayHdr {
            target_id,
            width,
            height,
            refresh_mhz,
            enable_hdr,
        } => {
            let _ =
                ipc_client::send_ipc_checked(&IpcRequest::DisplaySetMode(DisplaySetModeRequest {
                    client_id: None,
                    target_id: Some(target_id),
                    width,
                    height,
                    refresh_mhz,
                    hdr: Some(enable_hdr),
                    ..Default::default()
                }))?;
        }
        MenuAction::SwitchDisplayMode {
            target_id,
            width,
            height,
            refresh_mhz,
        } => {
            let _ =
                ipc_client::send_ipc_checked(&IpcRequest::DisplaySetMode(DisplaySetModeRequest {
                    client_id: None,
                    target_id: Some(target_id),
                    width,
                    height,
                    refresh_mhz,
                    hdr: None,
                    ..Default::default()
                }))?;
        }
        MenuAction::SwitchGpu { selector } => {
            let _ = ipc_client::send_ipc_checked(&IpcRequest::SwitchGpu(SwitchGpuRequest {
                selector,
            }))?;
        }
        MenuAction::StopSunshineSession => {
            let _ = ipc_client::send_ipc_checked(&IpcRequest::StopSession(StopSessionRequest {
                client_id: None,
                owner_pid: None,
                force: false,
            }))?;
        }
        MenuAction::ConfigureSunshine => {
            let exe = find_zerovdd_cli_exe();
            shell_execute(
                hwnd,
                "runas",
                &exe.to_string_lossy(),
                Some("sunshine configure --apply --clear-output-name"),
            )?;
        }
        MenuAction::RestorePhysicalDisplays => {
            let _ = ipc_client::send_ipc_checked(&IpcRequest::RestoreTopology)?;
        }
        MenuAction::OpenConfigToml => {
            let path = default_config_path();
            if !path.exists() {
                save_atomic(&path, &ConfigV1::default()).with_context(|| {
                    format!("failed to create default config at `{}`", path.display())
                })?;
            }
            shell_execute(hwnd, "open", &path.to_string_lossy(), None)?;
        }
        MenuAction::CopyDoctorJson => {
            let exe = find_zerovdd_cli_exe();
            let output = Command::new(&exe)
                .args(["doctor", "--json"])
                .creation_flags(CREATE_NO_WINDOW_FLAG)
                .output()
                .with_context(|| format!("failed to run `{}` doctor --json", exe.display()))?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                bail!(
                    "zerovdd doctor --json failed ({}): {}",
                    output.status,
                    stderr.trim()
                );
            }
            let report = String::from_utf8_lossy(&output.stdout);
            copy_text_to_clipboard(hwnd, &report)?;
        }
        MenuAction::ToggleAutostart => {
            let next = !is_autostart_enabled();
            set_autostart_enabled(next)?;
        }
        MenuAction::Exit => {
            remove_tray_icon(hwnd);
            // SAFETY: Destroying `hwnd` posts `WM_DESTROY` and cleanly exits `GetMessageW`.
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
        }
    }
    Ok(())
}

fn find_zerovdd_cli_exe() -> PathBuf {
    if let Ok(current) = std::env::current_exe() {
        if let Some(dir) = current.parent() {
            let candidate = dir.join("zerovdd.exe");
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    PathBuf::from("zerovdd.exe")
}

fn shell_execute(hwnd: HWND, verb: &str, file: &str, args: Option<&str>) -> Result<()> {
    let wide_verb = to_wide(verb);
    let wide_file = to_wide(file);
    let wide_args = args.map(to_wide);
    let args_ptr = wide_args
        .as_ref()
        .map_or(PCWSTR::null(), |buf| PCWSTR(buf.as_ptr()));
    // SAFETY: All NUL-terminated UTF-16 pointers live across the synchronous `ShellExecuteW` call.
    let result = unsafe {
        ShellExecuteW(
            Some(hwnd),
            PCWSTR(wide_verb.as_ptr()),
            PCWSTR(wide_file.as_ptr()),
            args_ptr,
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if (result.0 as isize) <= 32 {
        bail!(
            "ShellExecuteW(`{verb}`, `{file}`) failed with code {}",
            result.0 as isize
        );
    }
    Ok(())
}

fn copy_text_to_clipboard(hwnd: HWND, text: &str) -> Result<()> {
    let wide = to_wide(text);
    let byte_len = wide.len().saturating_mul(size_of::<u16>());

    // SAFETY: Allocates moveable clipboard memory of `byte_len` bytes.
    let hmem = unsafe { GlobalAlloc(GMEM_MOVEABLE, byte_len) }.context("GlobalAlloc failed")?;
    // SAFETY: Locks newly allocated `hmem` pointer exclusively on this thread.
    let locked = unsafe { GlobalLock(hmem) } as *mut u16;
    if locked.is_null() {
        // SAFETY: Free `hmem` when locking fails before ownership passes to the clipboard.
        unsafe {
            let _ = GlobalFree(Some(hmem));
        }
        bail!("GlobalLock returned null");
    }
    // SAFETY: `locked` has room for `wide.len()` UTF-16 code units matching `byte_len`.
    unsafe {
        core::ptr::copy_nonoverlapping(wide.as_ptr(), locked, wide.len());
        let _ = GlobalUnlock(hmem);
    }

    // SAFETY: Opens clipboard owned by `hwnd`; closed unconditionally below.
    unsafe { OpenClipboard(Some(hwnd)) }.context("OpenClipboard failed")?;
    let set_res = (|| -> Result<()> {
        // SAFETY: Clipboard is open on this thread.
        unsafe { EmptyClipboard() }.context("EmptyClipboard failed")?;
        // SAFETY: Clipboard owns `hmem` once `SetClipboardData` succeeds.
        unsafe { SetClipboardData(CF_UNICODETEXT_FORMAT, Some(HANDLE(hmem.0))) }
            .context("SetClipboardData(CF_UNICODETEXT) failed")?;
        Ok(())
    })();
    // SAFETY: Always close clipboard opened above.
    unsafe {
        let _ = CloseClipboard();
    }

    if let Err(err) = set_res {
        // SAFETY: Reclaim `hmem` if `SetClipboardData` did not take ownership.
        unsafe {
            let _ = GlobalFree(Some(hmem));
        }
        return Err(err);
    }
    Ok(())
}

fn is_autostart_enabled() -> bool {
    let subkey = to_wide(RUN_REG_KEY);
    let value_name = to_wide(RUN_VALUE_NAME);
    let mut hkey = HKEY::default();
    // SAFETY: `subkey` is NUL-terminated UTF-16 and `hkey` receives the opened handle.
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            Some(0),
            KEY_READ,
            &mut hkey,
        )
    };
    if status != ERROR_SUCCESS {
        return false;
    }

    let mut data_len: u32 = 0;
    // SAFETY: `hkey` is open for reading and `value_name` is NUL-terminated.
    let query_status = unsafe {
        RegQueryValueExW(
            hkey,
            PCWSTR(value_name.as_ptr()),
            None,
            None,
            None,
            Some(&mut data_len),
        )
    };
    // SAFETY: Closes opened registry key handle.
    unsafe {
        let _ = RegCloseKey(hkey);
    }
    query_status == ERROR_SUCCESS && data_len > 2
}

fn set_autostart_enabled(enabled: bool) -> Result<()> {
    let subkey = to_wide(RUN_REG_KEY);
    let value_name = to_wide(RUN_VALUE_NAME);
    let mut hkey = HKEY::default();
    // SAFETY: Creates or opens `HKCU\...\Run` with write permission.
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            Some(0),
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut hkey,
            None,
        )
    };
    if status != ERROR_SUCCESS {
        bail!("RegCreateKeyExW failed with error {}", status.0);
    }

    let write_result = if enabled {
        let exe = std::env::current_exe().context("std::env::current_exe failed")?;
        let quoted = format!("\"{}\"", exe.display());
        let wide = to_wide(&quoted);
        // SAFETY: `wide` outlives `RegSetValueExW`; pass exact UTF-16 byte slice.
        let bytes = unsafe {
            core::slice::from_raw_parts(
                wide.as_ptr() as *const u8,
                wide.len().saturating_mul(size_of::<u16>()),
            )
        };
        // SAFETY: `hkey` is writable and `value_name` is NUL-terminated.
        unsafe {
            RegSetValueExW(
                hkey,
                PCWSTR(value_name.as_ptr()),
                Some(0),
                REG_SZ,
                Some(bytes),
            )
        }
    } else {
        // SAFETY: Deletes `ZeroVDD` Run value from writable `hkey`.
        let del = unsafe { RegDeleteValueW(hkey, PCWSTR(value_name.as_ptr())) };
        if del == ERROR_SUCCESS {
            ERROR_SUCCESS
        } else {
            del
        }
    };

    // SAFETY: Close `hkey` opened above.
    unsafe {
        let _ = RegCloseKey(hkey);
    }

    if write_result != ERROR_SUCCESS && enabled {
        bail!("RegSetValueExW failed with error {}", write_result.0);
    }
    Ok(())
}

fn show_error_dialog(hwnd: HWND, message: &str) {
    let title = to_wide("ZeroVDD");
    let body = to_wide(message);
    // SAFETY: Modal notification bound to hidden tray `hwnd` using valid UTF-16 buffers.
    unsafe {
        let _ = MessageBoxW(
            Some(hwnd),
            PCWSTR(body.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_OK | MB_ICONERROR,
        );
    }
}
