use std::sync::Mutex;
// O `sleep` do auto-hide da janela só existe no desktop.
#[cfg(desktop)]
use std::time::Duration;

use rusqlite::Connection;
// `Manager` é usado no mobile por `manage()` e `app.path()`; já `AppHandle`,
// `LogicalSize` e `WindowEvent` só fazem sentido nas funções de janela, todas
// anotadas com `#[cfg(desktop)]`.
use tauri::Manager;
#[cfg(desktop)]
use tauri::{AppHandle, LogicalSize, WindowEvent};

#[cfg(all(desktop, not(target_os = "linux")))]
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

#[cfg(all(desktop, not(target_os = "linux")))]
use tauri_plugin_autostart::ManagerExt;

mod db;
mod supabase;

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

/// Diz se há uma conta conectada e qual é.
#[tauri::command]
fn auth_status(state: tauri::State<DbState>) -> Result<Option<supabase::AuthInfo>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    let session = supabase::load_session(&conn).map_err(|e| e.to_string())?;
    Ok(session.map(supabase::AuthInfo::from))
}

/// Quantas alterações locais ainda não subiram para o servidor.
#[tauri::command]
fn pending_count(state: tauri::State<DbState>) -> Result<i64, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    db::pending_count(&conn).map_err(|e| e.to_string())
}

/// Dados para o frontend abrir o canal de Realtime do usuário conectado. Renova
/// o token se necessário, para o Realtime autorizar (RLS) o canal corretamente.
#[tauri::command]
async fn realtime_config(
    state: tauri::State<'_, DbState>,
) -> Result<Option<supabase::RealtimeConfig>, String> {
    let cfg = supabase::config().ok_or_else(|| "sync não configurado".to_string())?;
    supabase::realtime_config_fresh(&cfg, &state.0).await
}

/// Entra com email/senha e guarda os tokens. A senha nunca é persistida: só a
/// sessão (tokens) fica no banco local.
#[tauri::command]
async fn sign_in(
    state: tauri::State<'_, DbState>,
    email: String,
    password: String,
) -> Result<supabase::AuthInfo, String> {
    let cfg = supabase::config().ok_or_else(|| "sync não configurado".to_string())?;
    supabase::sign_in_and_store(&cfg, &state.0, &email, &password).await
}

/// Esquece a sessão local e apaga a cópia local dos dados (não mexe no
/// servidor). A cópia some junto para trocar de conta sem deixar para trás
/// linhas da conta anterior.
#[tauri::command]
fn sign_out(state: tauri::State<DbState>) -> Result<(), String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    supabase::forget_account(&conn).map_err(|e| e.to_string())
}

/// Dispara um ciclo de sincronização (pull → merge → push).
#[tauri::command]
async fn sync_now(state: tauri::State<'_, DbState>) -> Result<supabase::SyncReport, String> {
    let cfg = supabase::config().ok_or_else(|| "sync não configurado".to_string())?;
    supabase::sync(&cfg, &state.0).await
}
#[cfg(desktop)]
#[tauri::command]
fn quit(app: AppHandle) {
    exit_app(&app);
}
#[cfg(desktop)]
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

/// Diz se esta instalação consegue se auto-atualizar.
///
/// O updater grava no binário o tipo de pacote (`bundle_type`) em que ele foi
/// distribuído. Em desenvolvimento o binário não pertence a nenhum pacote e a
/// atualização sobrescreveria o próprio executável em execução, então só
/// liberamos quando há um bundle conhecido: AppImage no Linux (sem senha) e
/// `.deb`/`.rpm` (pede senha de administrador via pkexec/zenity), `.msi`/`.exe`
/// no Windows e `.app` no macOS.
#[tauri::command]
fn can_self_update() -> bool {
    tauri::utils::platform::bundle_type().is_some()
}

#[cfg(desktop)]
/// Ícone da bandeja, embutido no binário a partir de `icons/newicon`. Fica
/// separado do ícone do app/instalador (`bundle.icon` no `tauri.conf.json`).
fn tray_icon_image() -> tauri::Result<tauri::image::Image<'static>> {
    tauri::image::Image::from_bytes(include_bytes!("../icons/newicon/icon-256x256.png"))
}
#[cfg(desktop)]
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
#[cfg(desktop)]
pub fn exit_app(app: &AppHandle) {
    app.exit(0);
}
#[cfg(desktop)]
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

/// Liga/desliga o "Abrir ao inicializar" e sincroniza a marca do item de menu.
///
/// A marca é derivada do estado real do autostart em vez de uma alternância
/// cega, então continua correta mesmo se a entrada for mexida por fora.
#[cfg(all(desktop, not(target_os = "linux")))]
fn toggle_autostart(app: &AppHandle, item: &tauri::menu::CheckMenuItem<tauri::Wry>) {
    let autolaunch = app.autolaunch();
    let enabled = autolaunch.is_enabled().unwrap_or(false);

    let result = if enabled {
        autolaunch.disable()
    } else {
        autolaunch.enable()
    };

    match result {
        Ok(()) => {
            let _ = item.set_checked(!enabled);
        }
        Err(e) => {
            eprintln!("todo-taskbar: falha ao ajustar autostart: {e}");
            let _ = item.set_checked(enabled);
        }
    }
}
#[cfg(desktop)]
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

    position_in_work_area(window);
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
#[cfg(desktop)]
/// Ancora a janela no canto inferior direito da *área útil* do monitor (usado
/// quando a posição do ícone não está disponível).
///
/// A área útil é a tela menos a barra de tarefas. Ancorar no tamanho total do
/// monitor deixava a parte de baixo da janela embaixo da barra de tarefas no
/// Windows — o botão de criar aba, por exemplo, ficava inalcançável.
fn position_in_work_area(window: &tauri::WebviewWindow) {
    let Ok(Some(monitor)) = window.primary_monitor() else {
        return;
    };

    let Ok(size) = window.inner_size() else {
        return;
    };

    // `work_area` e `inner_size` já estão em pixels físicos; só o GAP é lógico.
    let area = monitor.work_area();
    let gap = (GAP * monitor.scale_factor()).round() as i32;

    let right = area.position.x + area.size.width as i32;
    let bottom = area.position.y + area.size.height as i32;
    let x = (right - size.width as i32 - gap).max(area.position.x);
    let y = (bottom - size.height as i32 - gap).max(area.position.y);

    let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
}
#[cfg(desktop)]
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
    // Em desenvolvimento as credenciais públicas vêm do `.env`; no app
    // empacotado valem as embutidas em tempo de compilação (se houver).
    let _ = dotenvy::dotenv();

    let mut builder = tauri::Builder::default().plugin(tauri_plugin_opener::init());

    // Tray, autostart e os plugins de atualização/reinício dependem de APIs que
    // só existem no desktop. Montar o builder por etapas (em vez de encadear
    // `.plugin(...)` com `#[cfg]` nos argumentos) deixa cada `if` inteiro
    // Some quando o alvo é mobile, em vez de virar uma chamada vazia.
    #[cfg(desktop)]
    {
        builder = builder
            .plugin(tauri_plugin_autostart::init(
                tauri_plugin_autostart::MacosLauncher::LaunchAgent,
                None,
            ))
            .plugin(tauri_plugin_process::init())
            .plugin(tauri_plugin_updater::Builder::new().build());
    }

    builder
        .invoke_handler(tauri::generate_handler![
            // Comandos que dependem de janela/bandeja: só existem no desktop,
            // então o frontend Android nunca os chama.
            #[cfg(desktop)]
            quit,
            #[cfg(desktop)]
            set_width,
            #[cfg(desktop)]
            can_self_update,
            load_state,
            save_state,
            auth_status,
            sign_in,
            sign_out,
            sync_now,
            realtime_config,
            pending_count
        ])
        .setup(|app| {
            #[cfg(desktop)]
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

            #[cfg(desktop)]
            let tray_icon = tray_icon_image()?;

            // No Linux usamos XEmbed (GtkStatusIcon) via FFI, porque o tray do
            // Tauri é StatusNotifierItem: sem menu o ícone não aparece e não há
            // como receber o botão direito.
            #[cfg(target_os = "linux")]
            tray_linux::create(app.handle(), &tray_icon);

            #[cfg(all(desktop, not(target_os = "linux")))]
            {
                let open = tauri::menu::MenuItem::with_id(app, "open", "Abrir", true, None::<&str>)?;
                let startup = tauri::menu::CheckMenuItem::with_id(
                    app,
                    "startup",
                    "Abrir ao inicializar",
                    true,
                    app.autolaunch().is_enabled().unwrap_or(false),
                    None::<&str>,
                )?;
                let quit = tauri::menu::MenuItem::with_id(app, "quit", "Sair", true, None::<&str>)?;
                let menu = tauri::menu::Menu::with_items(app, &[&open, &startup, &quit])?;

                TrayIconBuilder::new()
                    .icon(tray_icon.clone())
                    .tooltip("Todo Taskbar")
                    .menu(&menu)
                    // No Windows e no macOS o padrão é o menu abrir no clique
                    // esquerdo, o que impediria o clique de alternar a janela —
                    // comportamento diferente do Linux. Aqui o esquerdo abre a
                    // janela e o direito abre o menu, igual ao XEmbed.
                    .show_menu_on_left_click(false)
                    .on_menu_event(move |app, event| match event.id.as_ref() {
                        "open" => show_window(app),
                        "quit" => exit_app(app),
                        "startup" => toggle_autostart(app, &startup),
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
        .on_window_event(on_window_event)
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// No mobile a janela é a própria tela do app: não há bandeja para alinhar nem
/// "perder o foco" que devolva o usuário ao desktop, então o handler é vazio.
#[cfg(mobile)]
fn on_window_event(_window: &tauri::Window, _event: &tauri::WindowEvent) {}

/// Oculta a janela quando ela perde o foco e a realinha com o ícone da bandeja
/// quando o gerenciador de janelas redimensiona. São comportamentos de desktop:
/// no Android a janela é a própria tela do app.
#[cfg(desktop)]
fn on_window_event(window: &tauri::Window, event: &WindowEvent) {
    match event {
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
    }
}

#[cfg(test)]
mod tests {
    use super::tray_icon_image;

    /// O PNG da bandeja é embutido e decodificado em runtime; se sumir ou mudar
    /// de formato, o setup do app falharia. O teste avisa antes.
    #[test]
    fn tray_icon_decodes() {
        let icon = tray_icon_image().expect("PNG da bandeja deve decodificar");
        assert_eq!(icon.width(), 256);
        assert_eq!(icon.height(), 256);
        assert_eq!(icon.rgba().len(), 256 * 256 * 4);
    }
}
