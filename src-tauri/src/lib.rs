use std::sync::Mutex;
use std::time::Duration;

use rusqlite::Connection;
use tauri::{AppHandle, LogicalPosition, LogicalSize, Manager, WindowEvent};

#[cfg(not(target_os = "linux"))]
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

mod db;

#[cfg(target_os = "linux")]
mod tray_linux;

/// Estado compartilhado entre os comandos: uma única conexão, protegida por
/// mutex para o WebView nunca escrever por cima de uma transação em andamento.
struct DbState(Mutex<Connection>);

/// Distância em px lógicos entre a janela e a borda do monitor.
const GAP: f64 = 8.0;

/// Largura mínima da janela, em px lógicos.
const MIN_WIDTH: f64 = 300.0;

/// Fração da largura da tela que a janela pode ocupar.
const MAX_WIDTH_RATIO: f64 = 0.25;

/// Lê abas e TODOs do banco.
#[tauri::command]
fn load_state(state: tauri::State<DbState>) -> Result<db::Store, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    db::load_state(&conn).map_err(|e| e.to_string())
}

/// Grava o estado inteiro e devolve o que ficou no banco, já com o
/// `completed_at` das conclusões calculado no fuso de Brasília.
#[tauri::command]
fn save_state(
    state: tauri::State<DbState>,
    store: db::IncomingStore,
) -> Result<db::Store, String> {
    let mut conn = state.0.lock().map_err(|e| e.to_string())?;
    db::save_state(&mut conn, store).map_err(|e| e.to_string())
}

#[tauri::command]
fn quit(app: AppHandle) {
    exit_app(&app);
}

/// Redimensiona a janela só na horizontal, mantendo a altura e a borda direita
/// presas no ícone da bandeja. O teto de largura é imposto por
/// [`configure_resize`] via `set_max_size`, então qualquer valor acima é
/// descartado pelo GTK antes de chegar aqui.
#[tauri::command]
fn set_width(app: AppHandle, width: u32) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let Ok(size) = window.inner_size() else {
        return;
    };
    if width == 0 || width == size.width {
        return;
    }

    let _ = window.set_size(tauri::PhysicalSize::new(width, size.height));

    if window.is_visible().unwrap_or(false) {
        position_window(&window);
    }

}

/// Trava a altura e limita a largura a um quarto da tela, para a janela não
/// virar uma faixa gigante em monitores largos.
fn configure_resize(window: &tauri::WebviewWindow) {
    let Ok(Some(monitor)) = window.current_monitor().or_else(|_| window.primary_monitor()) else {
        return;
    };
    let Ok(inner) = window.inner_size() else {
        return;
    };

    let scale = monitor.scale_factor();
    let screen_width = monitor.size().width as f64 / scale;
    let max_width = (screen_width * MAX_WIDTH_RATIO).floor().max(MIN_WIDTH);
    let height = inner.height as f64 / scale;

    let _ = window.set_min_size(Some(LogicalSize::new(MIN_WIDTH, height)));
    let _ = window.set_max_size(Some(LogicalSize::new(max_width, height)));

    // Telas estreitas podem ter um quarto menor que a largura inicial.
    let width = inner.width as f64 / scale;
    if width > max_width {
        let _ = window.set_size(LogicalSize::new(max_width, height));
    }
}

pub fn exit_app(app: &AppHandle) {
    app.exit(0);
}

pub fn show_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };

    // `outer_size()` só é confiável depois que a janela é realizada, então
    // mostramos antes de dimensionar o posicionamento.
    let _ = window.show();
    position_window(&window);
    let _ = window.set_focus();
}

/// Ancora a janela logo acima do ícone da bandeja, alinhada à borda direita
/// dele. Se o painel não informar a posição do ícone, cai para o canto do
/// monitor.
fn position_window(window: &tauri::WebviewWindow) {
    #[cfg(target_os = "linux")]
    if let Some((icon_x, icon_y, icon_width, _)) = tray_linux::icon_rect() {
        if position_above_icon(window, icon_x, icon_y, icon_width) {
            return;
        }
    }

    position_near_corner(window);
}

/// Posiciona a janela acima do ícone, com a borda direita alinhada à dele.
/// Todas as coordenadas e tamanhos são físicos.
#[cfg(target_os = "linux")]
fn position_above_icon(
    window: &tauri::WebviewWindow,
    icon_x: i32,
    icon_y: i32,
    icon_width: i32,
) -> bool {
    // `outer_size` no Linux devolve valores errados para janela sem decorações
    // (aqui, 800x300 em vez de 320x440); `inner_size` é o que reflete o layout.
    let Ok(size) = window.inner_size() else {
        return false;
    };

    let gap = GAP.round() as i32;
    let x = icon_x + icon_width - size.width as i32;
    let y = icon_y - size.height as i32 - gap;

    // Some resumos em painéis Wayland/GTK costumam devolver (0, 0) para o ícone;
    // nesse caso o alinhamento não faz sentido.
    if icon_x <= 0 && icon_y <= 0 {
        return false;
    }

    window
        .set_position(tauri::PhysicalPosition::new(x, y))
        .is_ok()
}

/// Ancora a janela no canto inferior direito do monitor (usado quando a posição
/// do ícone não está disponível).
fn position_near_corner(window: &tauri::WebviewWindow) {
    let Ok(Some(monitor)) = window.primary_monitor() else {
        return;
    };

    let Ok(size) = window.inner_size() else {
        return;
    };

    let scale = monitor.scale_factor();
    let position = monitor.position();
    let monitor_size = monitor.size();

    let width = size.width as f64 / scale;
    let height = size.height as f64 / scale;
    let monitor_x = position.x as f64 / scale;
    let monitor_y = position.y as f64 / scale;
    let monitor_width = monitor_size.width as f64 / scale;
    let monitor_height = monitor_size.height as f64 / scale;

    let x = monitor_x + monitor_width - width - GAP;
    let y = monitor_y + monitor_height - height - GAP;

    let _ = window.set_position(LogicalPosition::new(x, y));
}

pub fn toggle_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };

    if window.is_visible().unwrap_or(false) && window.is_focused().unwrap_or(false) {
        let _ = window.hide();
    } else {
        show_window(app);
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![quit, set_width, load_state, save_state])
        .setup(|app| {
            if let Some(window) = app.get_webview_window("main") {
                configure_resize(&window);
            }

            // O banco fica no diretório de dados do app, junto dos demais
            // arquivos que o Tauri já gerencia por plataforma.
            let path = app
                .path()
                .app_data_dir()
                .map_err(|e| e.to_string())?
                .join("todobar.db");
            let conn = db::open(&path).map_err(|e| e.to_string())?;
            app.manage(DbState(Mutex::new(conn)));

            // No Linux usamos XEmbed (GtkStatusIcon) via FFI, porque o tray do
            // Tauri é StatusNotifierItem: sem menu o ícone não aparece e não há
            // como receber o botão direito.
            #[cfg(target_os = "linux")]
            tray_linux::create(app.handle());

            #[cfg(not(target_os = "linux"))]
            {
                let open = tauri::menu::MenuItem::with_id(app, "open", "Abrir", true, None::<&str>)?;
                let quit = tauri::menu::MenuItem::with_id(app, "quit", "Sair", true, None::<&str>)?;
                let menu = tauri::menu::Menu::with_items(app, &[&open, &quit])?;

                TrayIconBuilder::new()
                    .icon(app.default_window_icon().unwrap().clone())
                    .tooltip("todobar")
                    .menu(&menu)
                    .on_menu_event(|app, event| match event.id.as_ref() {
                        "open" => show_window(app),
                        "quit" => exit_app(app),
                        _ => {}
                    })
                    .on_tray_icon_event(|tray, event| {
                        if let TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        } = event
                        {
                            toggle_window(tray.app_handle());
                        }
                    })
                    .build(app)?;
            }

            Ok(())
        })
        .on_window_event(|window, event| match event {
            WindowEvent::Focused(false) => {
                let app = window.clone();
                std::thread::spawn(move || {
                    // Espera um instante para não esconder durante recarregamentos
                    // do frontend (dev/HMR), que derrubam o foco temporariamente.
                    std::thread::sleep(Duration::from_millis(250));
                    let focused = app
                        .get_webview_window("main")
                        .and_then(|w| w.is_focused().ok())
                        .unwrap_or(false);
                    if !focused {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.hide();
                        }
                    }
                });
            }
            WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                let _ = window.hide();
            }
            // Se a janela for redimensionada por outro caminho (ex.: pelo WM),
            // realinha com o ícone para não descolar da bandeja.
            WindowEvent::Resized(_) => {
                if !window.is_visible().unwrap_or(false) {
                    return;
                }
                let app = window.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(200));
                    if let Some(window) = app.get_webview_window("main") {
                        if window.is_visible().unwrap_or(false) {
                            position_window(&window);
                        }
                    }
                });
            }
            _ => {}
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
