#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod account;
mod proxy;

use account::Account;
use serde::Serialize;
use std::sync::Mutex;
use tauri::{
    menu::{Menu, MenuItemBuilder, SubmenuBuilder},
    AppHandle, Manager, Url, WebviewWindow,
};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
use tauri_plugin_updater::UpdaterExt;

/// Pagina di accesso inclusa nell'app, per tornarci dal menu.
struct LoginUrl(Url);
/// Proxy attivo: va fermato quando si cambia account.
struct ProxyTask(Mutex<Option<tokio::task::JoinHandle<()>>>);

#[derive(Serialize)]
struct SavedAccount {
    url: String,
    username: String,
    has_password: bool,
}

fn config_dir(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    app.path().app_config_dir().map_err(|e| e.to_string())
}

#[tauri::command]
fn get_account(app: AppHandle) -> Option<SavedAccount> {
    let acc = account::load(&config_dir(&app).ok()?)?;
    Some(SavedAccount { has_password: acc.password().is_some(), url: acc.url, username: acc.username })
}

async fn open(app: &AppHandle, window: &WebviewWindow, acc: &Account, password: &str) -> Result<(), String> {
    let up = proxy::Upstream::new(&acc.url, &acc.username, password)?;
    proxy::check(&up).await?;
    let (local, task) = proxy::start(up).await?;
    if let Some(old) = app.state::<ProxyTask>().0.lock().unwrap().replace(task) {
        old.abort();
    }
    window.navigate(local.parse().map_err(|_| "indirizzo locale non valido")?).map_err(|e| e.to_string())
}

/// Accesso dal modulo: verifica, salva e apre il terminale.
#[tauri::command]
async fn connect(
    app: AppHandle,
    window: WebviewWindow,
    url: String,
    username: String,
    password: String,
    remember: bool,
) -> Result<(), String> {
    let acc = Account { url: account::normalize_url(&url)?, username: username.trim().to_string() };
    // campo vuoto = usa quella nel portachiavi
    let password = if password.is_empty() { acc.password().unwrap_or_default() } else { password };
    if acc.username.is_empty() || password.is_empty() {
        return Err("inserisci nome utente e password".into());
    }
    let up = proxy::Upstream::new(&acc.url, &acc.username, &password)?;
    proxy::check(&up).await?;
    account::save(&config_dir(&app)?, &acc)?;
    if remember {
        acc.store_password(&password)?;
    } else {
        acc.forget_password();
    }
    open(&app, &window, &acc, &password).await
}

/// Accesso automatico con le credenziali salvate.
#[tauri::command]
async fn connect_saved(app: AppHandle, window: WebviewWindow) -> Result<(), String> {
    let acc = account::load(&config_dir(&app)?).ok_or("nessun account salvato")?;
    let password = acc.password().ok_or("password non salvata")?;
    open(&app, &window, &acc, &password).await
}

#[tauri::command]
fn forget(app: AppHandle) -> Result<(), String> {
    if let Some(acc) = account::load(&config_dir(&app)?) {
        acc.forget_password();
    }
    Ok(())
}

async fn ask(app: &AppHandle, text: String, ok: &str, cancel: &str) -> bool {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .message(text)
        .title("pi agent")
        .buttons(MessageDialogButtons::OkCancelCustom(ok.into(), cancel.into()))
        .show(move |yes| {
            let _ = tx.send(yes);
        });
    rx.await.unwrap_or(false)
}

fn tell(app: &AppHandle, text: String) {
    app.dialog().message(text).title("pi agent").show(|_| {});
}

async fn check_update(app: AppHandle, manual: bool) {
    let result = async {
        let Some(update) = app.updater().map_err(|e| e.to_string())?.check().await.map_err(|e| e.to_string())? else {
            if manual {
                tell(&app, format!("Hai già l'ultima versione ({}).", app.package_info().version));
            }
            return Ok(());
        };
        let text = format!(
            "È disponibile la versione {} (installata: {}). Installarla adesso? L'app si riavvierà.",
            update.version, update.current_version
        );
        if ask(&app, text, "Installa", "Più tardi").await {
            update.download_and_install(|_, _| {}, || {}).await.map_err(|e| e.to_string())?;
            app.restart();
        }
        Ok::<(), String>(())
    }
    .await;
    if let Err(e) = result {
        if manual {
            tell(&app, format!("Controllo aggiornamenti non riuscito: {e}"));
        }
    }
}

fn build_menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let pi = SubmenuBuilder::new(app, "pi")
        .item(&MenuItemBuilder::with_id("reload", "Ricarica").accelerator("CmdOrCtrl+Shift+R").build(app)?)
        .item(&MenuItemBuilder::with_id("account", "Account…").build(app)?)
        .separator()
        .item(&MenuItemBuilder::with_id("update", "Controlla aggiornamenti…").build(app)?)
        .build()?;
    // Su macOS il menu predefinito serve per Cmd+C/Cmd+V. Su Windows no: le
    // sue scorciatoie Ctrl+C/Ctrl+V toglierebbero i tasti al terminale.
    #[cfg(target_os = "macos")]
    let menu = {
        let m = Menu::default(app)?;
        m.append(&pi)?;
        m
    };
    #[cfg(not(target_os = "macos"))]
    let menu = Menu::with_items(app, &[&pi])?;
    Ok(menu)
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(ProxyTask(Mutex::new(None)))
        .invoke_handler(tauri::generate_handler![get_account, connect, connect_saved, forget])
        .setup(|app| {
            let window = app.get_webview_window("main").expect("finestra main");
            app.manage(LoginUrl(window.url()?));
            app.set_menu(build_menu(app.handle())?)?;
            app.on_menu_event(|app, ev| {
                let Some(w) = app.get_webview_window("main") else { return };
                match ev.id().as_ref() {
                    "reload" => {
                        let _ = w.eval("location.reload()");
                    }
                    "account" => {
                        let mut u = app.state::<LoginUrl>().0.clone();
                        u.set_query(Some("manual=1"));
                        let _ = w.navigate(u);
                    }
                    "update" => {
                        tauri::async_runtime::spawn(check_update(app.clone(), true));
                    }
                    _ => {}
                }
            });
            tauri::async_runtime::spawn(check_update(app.handle().clone(), false));
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("avvio dell'app");
}
