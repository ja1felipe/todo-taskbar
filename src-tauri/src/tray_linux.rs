//! Ícone de bandeja via XEmbed (`GtkStatusIcon`) em vez do StatusNotifierItem
//! usado pelo Tauri.
//!
//! O SNI é um protocolo D-Bus em que o painel desenha o ícone e só emite
//! `Activate` (clique principal) e `SecondaryActivate` (clique do meio) — não
//! existe botão direito. Já o XEmbed embute uma janela X11 no painel e entrega
//! os eventos de mouse crus, permitindo abrir a janela no clique esquerdo e um
//! menu no direito.
//!
//! O wrapper seguro `gtk::StatusIcon` foi removido do gtk-rs, mas o `gtk-sys`
//! ainda expõe a API C completa, então usamos FFI diretamente.

use std::ffi::{c_int, c_uint, CString};
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

use gdk_pixbuf_sys::{gdk_pixbuf_new_from_data, GdkPixbuf, GDK_COLORSPACE_RGB};
use gdk_sys::{GdkEvent, GdkEventButton};
use glib_sys::{gboolean, gpointer};
use gobject_sys::{g_object_unref, g_signal_connect_data, GCallback, GObject};
use gtk_sys::{
    gtk_check_menu_item_get_active, gtk_check_menu_item_new_with_label,
    gtk_check_menu_item_set_active, gtk_menu_item_new_with_label, gtk_menu_new,
    gtk_menu_popup_at_pointer, gtk_menu_shell_append, gtk_separator_menu_item_new,
    gtk_status_icon_get_x11_window_id, gtk_status_icon_new, gtk_status_icon_set_from_pixbuf,
    gtk_status_icon_set_tooltip_text, gtk_status_icon_set_visible, gtk_widget_show_all,
    GtkCheckMenuItem, GtkMenu, GtkMenuItem, GtkMenuShell, GtkStatusIcon, GtkWidget,
};
use x11::xlib::{XGetGeometry, XTranslateCoordinates, XDefaultRootWindow};

use tauri::AppHandle;
use tauri_plugin_autostart::ManagerExt;

use crate::{exit_app, show_window, toggle_window};

const PRIMARY_BUTTON: c_uint = 1;
const SECONDARY_BUTTON: c_uint = 3;

/// Ponteiro para o `GtkStatusIcon`, para consultar a posição real do ícone.
static ICON: AtomicPtr<GtkStatusIcon> = AtomicPtr::new(std::ptr::null_mut());

/// Guarda contra reentrância no handler do "Abrir ao inicializar": reverter a
/// marca chama `gtk_check_menu_item_set_active`, que reemite "toggled".
static APPLYING_STARTUP: AtomicBool = AtomicBool::new(false);

// Não é exposto pelo `gdk-sys`, mas existe na libgdk-3.
extern "C" {
    fn gdk_x11_get_default_xdisplay() -> *mut x11::xlib::Display;
}

/// Retorna `(x, y, largura, altura)` do ícone na tela, em pixels físicos.
///
/// Usado para ancorar a janela colada no ícone, em vez de estimar pelo canto
/// do monitor.
///
/// `gtk_status_icon_get_geometry` não serve: ele depende de `_NET_SYSTEM_TRAY`
/// e devolve valores inúteis em painéis modernos (no Cinnamon retorna
/// `(0, 0, 1, 1)`). Como o ícone é uma janela X11 real (XEmbed), o caminho
/// confiável é pegar o XID e consultar a geometria via Xlib.
pub fn icon_rect() -> Option<(c_int, c_int, c_int, c_int)> {
    let icon = ICON.load(Ordering::Relaxed);
    if icon.is_null() {
        return None;
    }

    let xid = unsafe { gtk_status_icon_get_x11_window_id(icon) };
    if xid == 0 {
        return None;
    }

    let xid = xid as x11::xlib::XID;

    unsafe {
        let display = gdk_x11_get_default_xdisplay();
        if display.is_null() {
            return None;
        }

        let mut root = 0;
        let mut relative_x = 0;
        let mut relative_y = 0;
        let mut width = 0;
        let mut height = 0;
        let mut border_width = 0;
        let mut depth = 0;

        let found = XGetGeometry(
            display,
            xid,
            &mut root,
            &mut relative_x,
            &mut relative_y,
            &mut width,
            &mut height,
            &mut border_width,
            &mut depth,
        );

        if found == 0 || width == 0 || height == 0 {
            return None;
        }

        // `XGetGeometry` devolve coordenadas relativas ao pai (o painel);
        // `XTranslateCoordinates` leva para o topo da tela.
        let mut screen_x = 0;
        let mut screen_y = 0;
        let mut child = 0;
        XTranslateCoordinates(
            display,
            xid,
            XDefaultRootWindow(display),
            0,
            0,
            &mut screen_x,
            &mut screen_y,
            &mut child,
        );

        Some((
            screen_x as c_int,
            screen_y as c_int,
            width as c_int,
            height as c_int,
        ))
    }
}

/// Estado compartilhado entre os callbacks de sinal, que são `extern "C"` e
/// portanto não conseguem capturar nada.
struct TrayContext {
    app: AppHandle,
    menu: *mut GtkMenu,
}

/// Cria o ícone de bandeja e o menu de contexto.
///
/// Precisa rodar na thread principal do GTK, que é onde o `tao` bomba o event
/// loop no Linux.
pub fn create(app: &AppHandle) {
    let icon = unsafe { gtk_status_icon_new() };
    if icon.is_null() {
        eprintln!("todo-taskbar: não foi possível criar o ícone de bandeja");
        return;
    }

    if let Some(image) = app.default_window_icon() {
        match load_pixbuf(image.rgba(), image.width(), image.height()) {
            Some(pixbuf) => unsafe {
                gtk_status_icon_set_from_pixbuf(icon, pixbuf);
                g_object_unref(pixbuf as *mut GObject);
            },
            None => eprintln!("todo-taskbar: não foi possível carregar o ícone"),
        }
    }

    unsafe {
        let tooltip = CString::new("Todo Taskbar").expect("tooltip sem NUL");
        gtk_status_icon_set_tooltip_text(icon, tooltip.as_ptr());
        gtk_status_icon_set_visible(icon, 1);
    }

    let (menu, open_item, startup_item, quit_item) = unsafe { build_menu() };

    // Deixa a marca do menu igual ao estado real do autostart. Precisa ser antes
    // de conectar o signal, senão o próprio ajuste dispararia o handler.
    let autostart_on = app.autolaunch().is_enabled().unwrap_or(false);
    unsafe {
        gtk_check_menu_item_set_active(
            startup_item as *mut GtkCheckMenuItem,
            if autostart_on { 1 } else { 0 },
        );
    }

ICON.store(icon, Ordering::Relaxed);

    // O contexto vive pelo processo inteiro (vazamento intencional), e o
    // GtkStatusIcon mantém sua própria referência — nunca é liberado.
    let context = Box::leak(Box::new(TrayContext {
        app: app.clone(),
        menu,
    }));
    let data = context as *mut TrayContext as gpointer;

    unsafe {
        g_signal_connect_data(
            open_item,
            c"activate".as_ptr(),
            to_callback(on_menu_open as *const ()),
            data,
            None,
            0,
        );
        g_signal_connect_data(
            startup_item,
            c"toggled".as_ptr(),
            to_callback(on_menu_startup as *const ()),
            data,
            None,
            0,
        );
        g_signal_connect_data(
            quit_item,
            c"activate".as_ptr(),
            to_callback(on_menu_quit as *const ()),
            data,
            None,
            0,
        );
        g_signal_connect_data(
            icon as *mut GObject,
            c"button-press-event".as_ptr(),
            to_callback(on_button_press as *const ()),
            data,
            None,
            0,
        );
    }
}

/// Converte RGBA em `GdkPixbuf`. O buffer de pixels é vazado de propósito,
/// porque o pixbuf guarda o ponteiro para ele.
fn load_pixbuf(rgba: &[u8], width: u32, height: u32) -> Option<*mut GdkPixbuf> {
    if rgba.is_empty() || width == 0 || height == 0 {
        return None;
    }

    let data = Box::leak(rgba.to_vec().into_boxed_slice());

    let pixbuf = unsafe {
        gdk_pixbuf_new_from_data(
            data.as_ptr(),
            GDK_COLORSPACE_RGB,
            1,
            8,
            width as c_int,
            height as c_int,
            (width * 4) as c_int,
            None,
            std::ptr::null_mut(),
        )
    };

    if pixbuf.is_null() {
        None
    } else {
        Some(pixbuf)
    }
}

/// Monta o menu de contexto exibido no botão direito, devolvendo o menu e os
/// itens que precisam receber signals.
///
/// Os geradores do gir devolvem `*mut GtkWidget` para `gtk_menu_new` e
/// `gtk_menu_item_new_with_label`, então é preciso converter para os tipos
/// usados nas demais assinaturas.
unsafe fn build_menu() -> (*mut GtkMenu, *mut GObject, *mut GObject, *mut GObject) {
    let menu = gtk_menu_new() as *mut GtkMenu;

    let open = gtk_menu_item_new_with_label(cstr("Abrir").as_ptr());
    let startup = gtk_check_menu_item_new_with_label(cstr("Abrir ao inicializar").as_ptr());
    let separator = gtk_separator_menu_item_new();
    let quit = gtk_menu_item_new_with_label(cstr("Sair").as_ptr());

    let shell = menu as *mut GtkMenuShell;
    gtk_menu_shell_append(shell, open as *mut GtkMenuItem);
    gtk_menu_shell_append(shell, startup as *mut GtkMenuItem);
    gtk_menu_shell_append(shell, separator as *mut GtkMenuItem);
    gtk_menu_shell_append(shell, quit as *mut GtkMenuItem);

    gtk_widget_show_all(menu as *mut GtkWidget);

    (
        menu,
        open as *mut GObject,
        startup as *mut GObject,
        quit as *mut GObject,
    )
}

fn cstr(text: &str) -> CString {
    CString::new(text).expect("texto sem NUL")
}

/// `GCallback` é `Option<extern "C" fn()>`, ou seja, sem aridade. O GLib
/// chama via `libffi` respeitando a assinatura real de cada signal, então
/// basta converter o ponteiro da nossa função para esse tipo genérico.
unsafe fn to_callback(handler: *const ()) -> GCallback {
    std::mem::transmute(handler)
}

/// `gboolean button_press_event(GtkStatusIcon*, GdkEventButton*, gpointer)`
extern "C" fn on_button_press(
    _icon: *mut GtkStatusIcon,
    event: *mut GdkEventButton,
    data: gpointer,
) -> gboolean {
    if event.is_null() {
        return 0;
    }

    let context = unsafe { &*(data as *const TrayContext) };
    let button = unsafe { (*event).button };

    match button {
        PRIMARY_BUTTON => toggle_window(&context.app),
        SECONDARY_BUTTON => unsafe {
            gtk_menu_popup_at_pointer(context.menu, event as *const GdkEvent);
        },
        _ => {}
    }

    1
}

/// `void activate(GtkMenuItem*, gpointer)`
extern "C" fn on_menu_open(_item: *mut GtkMenuItem, data: gpointer) {
    let context = unsafe { &*(data as *const TrayContext) };
    show_window(&context.app);
}

/// `void activate(GtkMenuItem*, gpointer)`
extern "C" fn on_menu_quit(_item: *mut GtkMenuItem, data: gpointer) {
    let context = unsafe { &*(data as *const TrayContext) };
    exit_app(&context.app);
}

/// `void toggled(GtkCheckMenuItem*, gpointer)`
///
/// O GTK já inverteu a marca antes de emitir "toggled", então basta ler o
/// estado do item e aplicá-lo ao autostart.
extern "C" fn on_menu_startup(item: *mut GtkMenuItem, data: gpointer) {
    if APPLYING_STARTUP.swap(true, Ordering::SeqCst) {
        return;
    }

    let context = unsafe { &*(data as *const TrayContext) };
    let active = unsafe { gtk_check_menu_item_get_active(item as *mut GtkCheckMenuItem) } != 0;
    let autolaunch = context.app.autolaunch();

    let result = if active {
        autolaunch.enable()
    } else {
        autolaunch.disable()
    };

    if let Err(e) = result {
        eprintln!("todo-taskbar: falha ao ajustar autostart: {e}");
        // Reverte a marca. A chamada reemite "toggled", mas o guarda acima faz
        // o handler reentrante sair na hora.
        unsafe {
            gtk_check_menu_item_set_active(
                item as *mut GtkCheckMenuItem,
                if active { 0 } else { 1 },
            );
        }
    }

    APPLYING_STARTUP.store(false, Ordering::SeqCst);
}