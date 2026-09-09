use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::TrayIconBuilder,
    App, AppHandle, Manager,
};

#[cfg(target_os = "windows")]
use tauri::tray::{MouseButton, MouseButtonState, TrayIconEvent};

use crate::desktop::MAIN_WINDOW_LABEL;

const OPEN_MENU_ID: &str = "tray-open";
const QUIT_MENU_ID: &str = "tray-quit";

pub fn create(app: &mut App) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, OPEN_MENU_ID, "打开 My Cursor", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, QUIT_MENU_ID, "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &separator, &quit])?;

    TrayIconBuilder::with_id("main")
        .icon(tauri::include_image!("./icons/32x32.png"))
        .tooltip("My Cursor")
        .menu(&menu)
        .show_menu_on_left_click(cfg!(target_os = "macos"))
        .on_menu_event(|app, event| match event.id().as_ref() {
            OPEN_MENU_ID => show_main_window(app),
            QUIT_MENU_ID => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            #[cfg(target_os = "windows")]
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }

            #[cfg(not(target_os = "windows"))]
            let _ = (tray, event);
        })
        .build(app)?;
    Ok(())
}

pub fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}
