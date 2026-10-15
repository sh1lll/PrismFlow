//! Menu-bar (tray) icon: PrismFlow keeps running when its window is closed,
//! so apps connected through it keep working. The menu shows the background
//! service, lets you switch a connection's model in one click, and holds
//! "Launch at login" and the real Quit.

use crate::service::Service;
use std::sync::Arc;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, Runtime};
use tauri_plugin_autostart::ManagerExt;

pub const TRAY_ID: &str = "prismflow";

pub fn show_window<R: Runtime>(app: &AppHandle<R>) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// Choices offered for switching one connection: every provider + model
/// already used by any connection that this app can also use.
pub fn switch_choices(svc: &Service, conn_id: &str) -> Vec<(String, String)> {
    let all = svc.connections().unwrap_or_default();
    let Some(me) = all.iter().find(|c| c.id == conn_id) else { return Vec::new() };
    let mut out: Vec<(String, String)> = Vec::new();
    for c in &all {
        let pair = (c.provider_id.clone(), c.model.clone());
        let usable = crate::providers::get(&c.provider_id).is_some_and(|p| crate::service::route_for(&me.app_id, p).is_some());
        if usable && pair != (me.provider_id.clone(), me.model.clone()) && !out.contains(&pair) {
            out.push(pair);
        }
    }
    out
}

fn build_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let svc = app.state::<Arc<Service>>().inner().clone();
    let status = svc.gateway_status();
    let conns = svc.connections().unwrap_or_default();
    let running = !status.url.is_empty();
    let line = if conns.is_empty() {
        "No apps connected".to_string()
    } else if running {
        format!("Background service: on · {} app{}", conns.len(), if conns.len() == 1 { "" } else { "s" })
    } else {
        format!("{} app{} connected", conns.len(), if conns.len() == 1 { "" } else { "s" })
    };
    let menu = Menu::new(app)?;
    menu.append(&MenuItem::with_id(app, "open", "Open PrismFlow", true, None::<&str>)?)?;
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(app, "status", line, false, None::<&str>)?)?;
    for c in &conns {
        let p = crate::providers::get(&c.provider_id).map(|p| p.display_name.to_string()).unwrap_or_else(|| c.provider_id.clone());
        let title = format!("{} → {} ({p})", crate::service::app_name(&c.app_id), c.model);
        let sub = Submenu::with_id(app, format!("conn:{}", c.id), title, true)?;
        let choices = switch_choices(&svc, &c.id);
        if choices.is_empty() {
            sub.append(&MenuItem::with_id(app, format!("none:{}", c.id), "Connect another model to switch here", false, None::<&str>)?)?;
        }
        for (pid, model) in choices {
            let pname = crate::providers::get(&pid).map(|p| p.display_name.to_string()).unwrap_or(pid.clone());
            sub.append(&MenuItem::with_id(app, format!("switch:{}|{pid}|{model}", c.id), format!("Use {model} ({pname})"), true, None::<&str>)?)?;
        }
        menu.append(&sub)?;
    }
    let profiles = svc.profiles();
    if !profiles.list.is_empty() {
        let title = format!("Profile: {}", profiles.active.clone().unwrap_or_else(|| "none".into()));
        let sub = Submenu::with_id(app, "profiles", title, true)?;
        for name in profiles.list.keys() {
            let mark = if profiles.active.as_deref() == Some(name.as_str()) { "✓ " } else { "" };
            sub.append(&MenuItem::with_id(app, format!("profile:{name}"), format!("{mark}{name}"), true, None::<&str>)?)?;
        }
        menu.append(&sub)?;
    }
    if !conns.is_empty() {
        menu.append(&MenuItem::with_id(app, "restart", "Restart background service", true, None::<&str>)?)?;
    }
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    let login = app.autolaunch().is_enabled().unwrap_or(false);
    menu.append(&CheckMenuItem::with_id(app, "login", "Launch at login", true, login, None::<&str>)?)?;
    menu.append(&MenuItem::with_id(app, "quit", "Quit PrismFlow", true, None::<&str>)?)?;
    Ok(menu)
}

pub fn refresh<R: Runtime>(app: &AppHandle<R>) {
    if let (Some(tray), Ok(menu)) = (app.tray_by_id(TRAY_ID), build_menu(app)) {
        let _ = tray.set_menu(Some(menu));
    }
}

/// Switches a connection to another provider + model: connect the new pair,
/// then (if the provider changed) remove the old connection.
pub async fn switch(svc: &Service, conn_id: &str, provider_id: &str, model: &str) -> Result<(), String> {
    let old = svc.connections()?.into_iter().find(|c| c.id == conn_id).ok_or("Unknown connection.")?;
    let new = svc.connect(&old.app_id, provider_id, model).await?;
    if new.id != old.id {
        svc.disconnect(&old.id).await?;
    }
    Ok(())
}

pub fn install<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let menu = build_menu(app)?;
    let icon = app.default_window_icon().cloned();
    let mut b = TrayIconBuilder::with_id(TRAY_ID).menu(&menu).tooltip("PrismFlow").show_menu_on_left_click(true);
    if let Some(i) = icon {
        b = b.icon(i).icon_as_template(cfg!(target_os = "macos"));
    }
    b.on_menu_event(|app, event| {
        let id = event.id().as_ref().to_string();
        let handle = app.clone();
        match id.as_str() {
            "open" => show_window(app),
            "quit" => {
                let svc = app.state::<Arc<Service>>().inner().clone();
                tauri::async_runtime::spawn(async move {
                    svc.diag.record(crate::diagnostics::Level::Info, "app.quit", "PrismFlow quit from the menu bar.");
                    handle.exit(0);
                });
            }
            "restart" => {
                let svc = app.state::<Arc<Service>>().inner().clone();
                tauri::async_runtime::spawn(async move {
                    let _ = svc.restart_gateway().await;
                    refresh(&handle);
                });
            }
            "login" => {
                let al = app.autolaunch();
                let _ = if al.is_enabled().unwrap_or(false) { al.disable() } else { al.enable() };
                refresh(app);
            }
            other if other.starts_with("profile:") => {
                let name = other.trim_start_matches("profile:").to_string();
                let svc = app.state::<Arc<Service>>().inner().clone();
                tauri::async_runtime::spawn(async move {
                    match svc.switch_profile(&name).await {
                        Ok(_) => crate::notify::send(&handle, "Profile switched", &format!("Now using \"{name}\".")),
                        Err(e) => crate::notify::send(&handle, "Profile switched with problems", &e),
                    }
                    refresh(&handle);
                });
            }
            other if other.starts_with("switch:") => {
                let rest = other.trim_start_matches("switch:").to_string();
                let parts: Vec<String> = rest.splitn(3, '|').map(String::from).collect();
                if parts.len() == 3 {
                    let svc = app.state::<Arc<Service>>().inner().clone();
                    tauri::async_runtime::spawn(async move {
                        if let Err(e) = switch(&svc, &parts[0], &parts[1], &parts[2]).await {
                            crate::notify::send(&handle, "Couldn't switch the model", &e);
                        }
                        refresh(&handle);
                    });
                }
            }
            _ => {}
        }
    })
    .build(app)?;
    Ok(())
}
