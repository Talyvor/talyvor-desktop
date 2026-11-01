//! Taly on the computer: a menu-bar icon on macOS and a tray icon on Windows; a global shortcut opens
//! a floating mini-chat with Taly. Approvals waiting are notified and opened on the web or the phone:
//! this app never approves anything itself.

pub mod connect;
pub mod mcp;

use std::{collections::HashSet, sync::Arc, time::Duration};
use tauri::{
    image::Image,
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, PhysicalPosition, RunEvent, WebviewWindow, WindowEvent, Wry,
};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_opener::OpenerExt;

pub const SHORTCUT: &str = "CommandOrControl+Shift+Space";
const POLL: Duration = Duration::from_secs(30);

type Talyvor<'a> = tauri::State<'a, Arc<mcp::Talyvor>>;

/// app.talyvor.com, or TALYVOR_URL for a local or test workspace.
fn base() -> String {
    std::env::var("TALYVOR_URL").map(|u| u.trim_end_matches('/').to_owned()).unwrap_or_else(|_| "https://app.talyvor.com".into())
}

#[tauri::command]
async fn status(t: Talyvor<'_>) -> Result<bool, String> {
    Ok(t.signed_in().await)
}

#[tauri::command]
async fn sign_in(app: AppHandle, t: Talyvor<'_>) -> Result<(), String> {
    let opener = app.clone();
    t.sign_in(move |url| {
        let _ = opener.opener().open_url(url, None::<&str>);
    })
    .await?;
    show(&app);
    Ok(())
}

#[tauri::command]
async fn sign_out(t: Talyvor<'_>) -> Result<(), String> {
    t.sign_out().await;
    Ok(())
}

#[tauri::command]
async fn ask(t: Talyvor<'_>, message: String, thread: Vec<mcp::Turn>) -> Result<mcp::Reply, String> {
    t.ask(&message, &thread).await
}

#[tauri::command]
async fn approvals(t: Talyvor<'_>) -> Result<Vec<mcp::Approval>, String> {
    t.approvals().await
}

#[tauri::command]
async fn transcribe(t: Talyvor<'_>, audio_base64: String, mime: String) -> Result<String, String> {
    t.transcribe(&audio_base64, &mime).await
}

/// Opens a Talyvor page (an approval, the web app) in the browser; any other address is refused.
#[tauri::command]
fn open_in_talyvor(app: AppHandle, t: Talyvor<'_>, url: String) -> Result<(), String> {
    if url != t.base && !url.starts_with(&format!("{}/", t.base)) {
        return Err("only Talyvor's own pages open from here".into());
    }
    app.opener().open_url(url, None::<&str>).map_err(|e| e.to_string())
}

#[tauri::command]
fn hide(window: WebviewWindow) {
    let _ = window.hide();
}

fn toggle(app: &AppHandle) {
    match app.get_webview_window("taly") {
        Some(w) if w.is_visible().unwrap_or(false) => {
            let _ = w.hide();
        }
        _ => show(app),
    }
}

/// Under the menu bar at the right on macOS, above the tray at the right on Windows.
fn show(app: &AppHandle) {
    let Some(w) = app.get_webview_window("taly") else { return };
    if let (Ok(Some(m)), Ok(size)) = (w.current_monitor(), w.outer_size()) {
        let (scale, area, at) = (m.scale_factor(), m.size(), m.position());
        let px = |logical: f64| (logical * scale) as i32;
        let x = at.x + area.width as i32 - size.width as i32 - px(16.0);
        let y = if cfg!(target_os = "macos") { at.y + px(36.0) } else { at.y + area.height as i32 - size.height as i32 - px(60.0) };
        let _ = w.set_position(PhysicalPosition::new(x, y));
    }
    let _ = w.show();
    let _ = w.set_focus();
    let _ = w.emit("taly-shown", ());
}

/// Every POLL: the approvals waiting, each new one notified once and counted in the tray menu.
fn watch_approvals(app: AppHandle, t: Arc<mcp::Talyvor>, count: MenuItem<Wry>) {
    tauri::async_runtime::spawn(async move {
        let mut seen = HashSet::new();
        loop {
            if t.signed_in().await {
                if let Ok(list) = t.approvals().await {
                    for a in mcp::new_approvals(&seen, &list) {
                        let _ = app.notification().builder()
                            .title("Approval waiting")
                            .body(format!("{} · {} — approve it on your phone or on the web.", a.title, mcp::money(a.amount_minor, &a.currency)))
                            .show();
                    }
                    seen = list.iter().map(|a| a.id.clone()).collect();
                    let _ = count.set_text(format!("Approvals waiting ({})", list.len()));
                    let _ = count.set_enabled(!list.is_empty());
                    let _ = app.emit("approvals", &list);
                }
            }
            tokio::time::sleep(POLL).await;
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let base = base();
    let talyvor = Arc::new(mcp::Talyvor::new(base.clone(), connect::load()));
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new()
            .with_handler(|app, _, e| {
                if e.state == ShortcutState::Pressed {
                    toggle(app)
                }
            })
            .build())
        .manage(talyvor.clone())
        .invoke_handler(tauri::generate_handler![status, sign_in, sign_out, ask, approvals, transcribe, open_in_talyvor, hide])
        .setup(move |app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let open = MenuItem::with_id(app, "open", "Open Taly", true, Some(SHORTCUT))?;
            let count = MenuItem::with_id(app, "approvals", "Approvals waiting (0)", false, None::<&str>)?;
            let web = MenuItem::with_id(app, "web", "Open Talyvor on the web", true, None::<&str>)?;
            let out = MenuItem::with_id(app, "sign-out", "Sign out", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit Talyvor", true, None::<&str>)?;
            let sep = || PredefinedMenuItem::separator(app);
            let menu = Menu::with_items(app, &[&open, &count, &web, &sep()?, &out, &quit])?;

            let tray = if cfg!(target_os = "macos") { &include_bytes!("../icons/tray-template.png")[..] } else { &include_bytes!("../icons/tray.png")[..] };
            let menu_base = base.clone();
            TrayIconBuilder::with_id("taly")
                .icon(Image::from_bytes(tray)?)
                .icon_as_template(cfg!(target_os = "macos"))
                .tooltip("Talyvor")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_tray_icon_event(|tray, e| {
                    if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                        toggle(tray.app_handle())
                    }
                })
                .on_menu_event(move |app, e| match e.id.as_ref() {
                    "open" => show(app),
                    "approvals" => {
                        let _ = app.opener().open_url(format!("{menu_base}/approvals"), None::<&str>);
                    }
                    "web" => {
                        let _ = app.opener().open_url(menu_base.clone(), None::<&str>);
                    }
                    "sign-out" => {
                        let (app, t) = (app.clone(), app.state::<Arc<mcp::Talyvor>>().inner().clone());
                        tauri::async_runtime::spawn(async move {
                            t.sign_out().await;
                            let _ = app.emit("signed-out", ());
                        });
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .build(app)?;

            if let Err(e) = app.global_shortcut().register(SHORTCUT) {
                // Another app holds the shortcut; the menu-bar icon still opens Taly.
                eprintln!("talyvor: the shortcut {SHORTCUT} is taken: {e}");
            }
            if let Some(w) = app.get_webview_window("taly") {
                let hide_on_blur = w.clone();
                w.on_window_event(move |e| match e {
                    WindowEvent::Focused(false) => {
                        let _ = hide_on_blur.hide();
                    }
                    WindowEvent::CloseRequested { api, .. } => {
                        api.prevent_close();
                        let _ = hide_on_blur.hide();
                    }
                    _ => {}
                });
            }
            watch_approvals(app.handle().clone(), talyvor.clone(), count);
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("Talyvor could not start")
        .run(|_, e| {
            // Hiding the last window keeps the app in the menu bar; only Quit ends it.
            if let RunEvent::ExitRequested { api, code: None, .. } = e {
                api.prevent_exit();
            }
        });
}
