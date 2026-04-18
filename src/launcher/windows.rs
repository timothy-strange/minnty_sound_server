use std::cell::RefCell;
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::thread;
use std::time::{Duration, Instant};

use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, TrayIconBuilder, TrayIconEvent};

use crate::i18n;

pub fn run_launcher() -> Result<(), Box<dyn std::error::Error>> {
    let menu = Menu::new();
    let heading = MenuItem::new(&i18n::text("launcher.menu.heading"), false, None);
    let show_ui = MenuItem::new(&i18n::text("launcher.menu.show_ui"), true, None);
    let quit = MenuItem::new(&i18n::text("launcher.menu.quit"), true, None);
    let separator = tray_icon::menu::PredefinedMenuItem::separator();
    menu.append(&heading)?;
    menu.append(&separator)?;
    menu.append(&show_ui)?;
    menu.append(&quit)?;

    let server_exe = std::env::current_exe()?;
    let child = match spawn_server_child(&server_exe) {
        Ok(child) => Some(child),
        Err(err) => {
            crate::log_warn!("launcher warning: failed to start server child on startup: {err}");
            None
        }
    };

    let icon = build_launcher_icon()?;
    let _tray = TrayIconBuilder::new()
        .with_tooltip(&i18n::text("launcher.tooltip"))
        .with_menu(Box::new(menu))
        .with_icon(icon)
        .build()?;
    crate::log_info!("launcher: tray icon created");

    let menu_rx = MenuEvent::receiver();
    let tray_rx = TrayIconEvent::receiver();
    let show_ui_id = show_ui.id().clone();
    let quit_id = quit.id().clone();
    let child_state = Rc::new(RefCell::new(child));
    crate::log_info!("launcher: ready (left-click tray icon or use menu)");

    if let Err(err) = ensure_server_running(&mut child_state.borrow_mut(), &server_exe) {
        crate::log_warn!("launcher warning: unable to ensure server running at startup: {err}");
    }
    let _ = wait_for_server_ready(Duration::from_secs(2));
    let _ = open::that("http://127.0.0.1:3000/");

    loop {
        reconcile_server_exit(&mut child_state.borrow_mut());

        while let Ok(event) = menu_rx.try_recv() {
            if event.id == show_ui_id {
                crate::log_info!("launcher: Show UI clicked");
                if let Err(err) = ensure_server_running(&mut child_state.borrow_mut(), &server_exe)
                {
                    crate::log_warn!("launcher warning: unable to ensure server running: {err}");
                }
                let _ = wait_for_server_ready(Duration::from_secs(2));
                let _ = open::that("http://127.0.0.1:3000/");
            } else if event.id == quit_id {
                crate::log_info!("launcher: Quit clicked");
                stop_server_child(&mut child_state.borrow_mut());
                return Ok(());
            }
        }

        while let Ok(event) = tray_rx.try_recv() {
            if matches!(event, TrayIconEvent::Click { .. }) {
                crate::log_info!("launcher: tray icon clicked -> Show UI");
                if let Err(err) = ensure_server_running(&mut child_state.borrow_mut(), &server_exe)
                {
                    crate::log_warn!("launcher warning: unable to ensure server running: {err}");
                }
                let _ = wait_for_server_ready(Duration::from_secs(2));
                let _ = open::that("http://127.0.0.1:3000/");
            }
        }

        thread::sleep(Duration::from_millis(20));
    }
}

fn spawn_server_child(server_exe: &std::path::Path) -> Result<Child, Box<dyn std::error::Error>> {
    crate::log_info!("launcher: starting server child");
    let child = Command::new(server_exe)
        .arg("--server")
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;
    crate::log_info!("launcher: server child started pid={}", child.id());
    Ok(child)
}

fn reconcile_server_exit(child: &mut Option<Child>) {
    if let Some(child_proc) = child.as_mut() {
        match child_proc.try_wait() {
            Ok(Some(status)) => {
                crate::log_warn!("launcher warning: server child exited: {status}");
                *child = None;
            }
            Ok(None) => {}
            Err(err) => {
                crate::log_warn!("launcher warning: failed to poll server child: {err}");
            }
        }
    }
}

fn ensure_server_running(
    child: &mut Option<Child>,
    server_exe: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    reconcile_server_exit(child);
    if child.is_none() {
        *child = Some(spawn_server_child(server_exe)?);
    }
    Ok(())
}

fn stop_server_child(child: &mut Option<Child>) {
    let Some(mut child_proc) = child.take() else {
        return;
    };

    match child_proc.try_wait() {
        Ok(Some(_)) => return,
        Ok(None) => {}
        Err(err) => {
            crate::log_warn!("launcher warning: failed to poll server child before stop: {err}");
        }
    }

    if let Err(err) = child_proc.kill() {
        crate::log_warn!("launcher warning: failed to kill server child: {err}");
    }
    if let Err(err) = child_proc.wait() {
        crate::log_warn!("launcher warning: failed to wait server child exit: {err}");
    }
    crate::log_info!("launcher: server child stopped");
}

fn wait_for_server_ready(timeout: Duration) -> bool {
    let addr: std::net::SocketAddr = match "127.0.0.1:3000".parse() {
        Ok(addr) => addr,
        Err(_) => return false,
    };

    let start = Instant::now();
    while start.elapsed() < timeout {
        if TcpStream::connect_timeout(&addr, Duration::from_millis(120)).is_ok() {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

fn build_launcher_icon() -> Result<Icon, Box<dyn std::error::Error>> {
    let width = 16u32;
    let height = 16u32;
    let mut rgba = vec![0u8; (width * height * 4) as usize];

    for y in 0..height {
        for x in 0..width {
            let idx = ((y * width + x) * 4) as usize;
            let is_border = x == 0 || y == 0 || x == width - 1 || y == height - 1;
            let is_center = (x > 4 && x < 11) && (y > 4 && y < 11);

            if is_border {
                rgba[idx] = 0x0f;
                rgba[idx + 1] = 0x17;
                rgba[idx + 2] = 0x2a;
                rgba[idx + 3] = 0xff;
            } else if is_center {
                rgba[idx] = 0x22;
                rgba[idx + 1] = 0xc5;
                rgba[idx + 2] = 0x5e;
                rgba[idx + 3] = 0xff;
            } else {
                rgba[idx] = 0x1f;
                rgba[idx + 1] = 0x29;
                rgba[idx + 2] = 0x37;
                rgba[idx + 3] = 0xff;
            }
        }
    }

    Ok(Icon::from_rgba(rgba, width, height)?)
}
