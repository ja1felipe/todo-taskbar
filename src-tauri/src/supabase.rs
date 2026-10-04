//! Cliente mínimo do Supabase (GoTrue para auth, PostgREST para dados) e o
//! motor de sincronização.
//!
//! O banco local continua sendo o dono da verdade. A cada sync:
//!
//!   1. `pull`   — baixa todas as linhas do usuário (RLS já filtra por conta).
//!   2. `merge`  — aplica o que for mais novo, por `updated_at` (last-write-wins).
//!   3. `push`   — sobe as linhas locais marcadas como `dirty`.
//!
//! O `updated_at` é gerado no cliente e enviado nas duas pontas, para que a
//! comparação seja maçã-com-maçã. Exclusões viajam como tombstone (`deleted`).

use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::db;

const META_ACCESS: &str = "supabase_access_token";
const META_REFRESH: &str = "supabase_refresh_token";
const META_EXPIRES: &str = "supabase_expires_at";
const META_USER_ID: &str = "supabase_user_id";
const META_EMAIL: &str = "supabase_email";
/// Instante do último sync concluído. Ausente = este banco nunca subiu nada.
const META_LAST_SYNC: &str = "supabase_last_sync";

/// Configuração pública do projeto. A `anon key` pode ser embutida no app: quem
/// limita o acesso são as políticas de RLS.
#[derive(Clone)]
pub struct Config {
    pub url: String,
    pub anon: String,
}

/// Lê a configuração do ambiente (carregado do `.env` em desenvolvimento) ou,
/// como fallback, do valor embutido em tempo de compilação.
pub fn config() -> Option<Config> {
    let url = env_or_build("SUPABASE_URL", option_env!("SUPABASE_URL")).or_else(|| {
        std::env::var("SUPABASE_API").ok().map(|u| {
            u.trim_end_matches('/')
                .trim_end_matches("/rest/v1")
                .trim_end_matches('/')
                .to_string()
        })
    })?;
    let anon = env_or_build("SUPABASE_ANON_KEY", option_env!("SUPABASE_ANON_KEY"))?;
    let url = url.trim_end_matches('/').to_string();
    if url.is_empty() || anon.is_empty() {
        return None;
    }
    Some(Config { url, anon })
}

fn env_or_build(key: &str, build: Option<&'static str>) -> Option<String> {
    std::env::var(key)
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| build.map(str::to_string))
}

/// Sessão persistida localmente. O `refresh_token` permite renovar o acesso sem
/// pedir a senha de novo.
#[derive(Serialize, Deserialize, Clone)]
pub struct Session {
    pub access_token: String,
    pub refresh_token: String,
    /// Instante de expiração do `access_token`, em segundos Unix.
    pub expires_at: i64,
    pub user_id: String,
    pub email: Option<String>,
}

/// Informação devolvida ao frontend sobre a conta conectada.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthInfo {
    pub user_id: String,
    pub email: Option<String>,
}

impl From<Session> for AuthInfo {
    fn from(s: Session) -> Self {
        AuthInfo {
            user_id: s.user_id,
            email: s.email,
        }
    }
}

/// Resumo de um ciclo de sincronização.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncReport {
    pub pulled_tabs: usize,
    pub pulled_todos: usize,
    pub pushed_tabs: usize,
    pub pushed_todos: usize,
}

/// O que o frontend precisa para abrir o canal de Realtime (WebSocket). O Rust
/// continua sendo o único dono do refresh do token; o cliente usa estes dados
/// só para se autenticar e escutar as mudanças.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RealtimeConfig {
    pub url: String,
    pub anon: String,
    pub access_token: String,
    pub user_id: String,
}

// ---------------------------------------------------------------------------
// Auth (GoTrue)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    expires_in: i64,
    user: AuthUser,
}

#[derive(Deserialize)]
struct AuthUser {
    id: String,
    email: Option<String>,
}

fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Login email/senha. A senha não é guardada — só os tokens resultantes.
pub async fn sign_in(cfg: &Config, email: &str, password: &str) -> Result<Session, String> {
    let resp = reqwest::Client::new()
        .post(format!("{}/auth/v1/token", cfg.url))
        .query(&[("grant_type", "password")])
        .header("apikey", &cfg.anon)
        .json(&serde_json::json!({ "email": email, "password": password }))
        .send()
        .await
        .map_err(|e| format!("falha ao falar com o Supabase: {e}"))?;

    parse_token(resp).await
}

/// Renova o acesso a partir do `refresh_token`.
pub async fn refresh(cfg: &Config, refresh_token: &str) -> Result<Session, String> {
    let resp = reqwest::Client::new()
        .post(format!("{}/auth/v1/token", cfg.url))
        .query(&[("grant_type", "refresh_token")])
        .header("apikey", &cfg.anon)
        .json(&serde_json::json!({ "refresh_token": refresh_token }))
        .send()
        .await
        .map_err(|e| format!("falha ao renovar a sessão: {e}"))?;

    parse_token(resp).await
}

async fn parse_token(resp: reqwest::Response) -> Result<Session, String> {
    let status = resp.status();
    let body = resp.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("login recusado ({}): {}", status.as_u16(), body));
    }

    let parsed: TokenResponse = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    Ok(Session {
        access_token: parsed.access_token,
        refresh_token: parsed.refresh_token,
        expires_at: now_unix() + parsed.expires_in,
        user_id: parsed.user.id,
        email: parsed.user.email,
    })
}

/// O token está perto de expirar? Renova com folga de 60s.
fn is_expiring(session: &Session) -> bool {
    session.expires_at <= now_unix() + 60
}

// ---------------------------------------------------------------------------
// Dados (PostgREST)
// ---------------------------------------------------------------------------

#[derive(Deserialize, Clone)]
struct RemoteTab {
    id: String,
    name: String,
    #[serde(default)]
    created_at: Option<String>,
    updated_at: String,
    #[serde(default)]
    deleted: bool,
}

#[derive(Deserialize, Clone)]
struct RemoteTodo {
    id: String,
    tab_id: String,
    text: String,
    #[serde(default)]
    done: bool,
    #[serde(default)]
    completed_at: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
    updated_at: String,
    #[serde(default)]
    deleted: bool,
}

struct RemoteData {
    tabs: Vec<RemoteTab>,
    todos: Vec<RemoteTodo>,
}

#[derive(Serialize)]
struct PushTab {
    id: String,
    name: String,
    created_at: String,
    updated_at: String,
    deleted: bool,
}

#[derive(Serialize)]
struct PushTodo {
    id: String,
    tab_id: String,
    text: String,
    done: bool,
    completed_at: Option<String>,
    created_at: String,
    updated_at: String,
    deleted: bool,
}

async fn get_json<T: DeserializeOwned>(
    cfg: &Config,
    session: &Session,
    table: &str,
) -> Result<T, String> {
    let resp = reqwest::Client::new()
        .get(format!("{}/rest/v1/{}", cfg.url, table))
        .query(&[("select", "*")])
        .header("apikey", &cfg.anon)
        .header("Authorization", format!("Bearer {}", session.access_token))
        .send()
        .await
        .map_err(|e| format!("falha ao baixar {table}: {e}"))?;

    let status = resp.status();
    let body = resp.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("supabase {table} ({}): {}", status.as_u16(), body));
    }
    serde_json::from_str(&body).map_err(|e| e.to_string())
}

async fn pull(cfg: &Config, session: &Session) -> Result<RemoteData, String> {
    Ok(RemoteData {
        tabs: get_json(cfg, session, "tabs").await?,
        todos: get_json(cfg, session, "todos").await?,
    })
}

async fn push_rows<T: Serialize>(
    cfg: &Config,
    session: &Session,
    table: &str,
    rows: &[T],
) -> Result<(), String> {
    let resp = reqwest::Client::new()
        .post(format!("{}/rest/v1/{}", cfg.url, table))
        .header("apikey", &cfg.anon)
        .header("Authorization", format!("Bearer {}", session.access_token))
        // `merge-duplicates` faz upsert pela chave primária (`id` = uuid).
        .header("Prefer", "resolution=merge-duplicates,return=minimal")
        .json(rows)
        .send()
        .await
        .map_err(|e| format!("falha ao enviar {table}: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("supabase {table} ({}): {}", status.as_u16(), body));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Merge
// ---------------------------------------------------------------------------

/// `true` se `remote` for estritamente mais recente que `local`. Os dois podem
/// vir em formatos diferentes (`...Z` do SQLite, `...+00:00` do Postgres), então
/// a comparação é feita nos instantes, não no texto. Se algum não parsear, cai
/// para a comparação de texto como último recurso.
fn is_newer(remote: &str, local: &str) -> bool {
    match (
        chrono::DateTime::parse_from_rfc3339(remote),
        chrono::DateTime::parse_from_rfc3339(local),
    ) {
        (Ok(r), Ok(l)) => r > l,
        _ => remote > local,
    }
}

fn merge(conn: &mut Connection, remote: &RemoteData) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    let now = db::now_utc();

    // As abas vêm antes porque os TODOs dependem do id local delas.
    let mut tab_map = std::collections::HashMap::new();
    for tab in &remote.tabs {
        let local_id = merge_tab(&tx, tab, &now)?;
        tab_map.insert(tab.id.clone(), local_id);
    }

    for todo in &remote.todos {
        if let Some(&local_tab) = tab_map.get(&todo.tab_id) {
            merge_todo(&tx, todo, local_tab, &now)?;
        }
    }

    tx.commit()
}

fn merge_tab(tx: &Transaction, remote: &RemoteTab, now: &str) -> rusqlite::Result<i64> {
    let existing: Option<(i64, String, i64)> = tx
        .query_row(
            "SELECT id, COALESCE(updated_at, ''), deleted FROM tabs WHERE uuid = ?1",
            params![remote.id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;

    match existing {
        None => {
            let created = remote
                .created_at
                .clone()
                .unwrap_or_else(|| now.to_string());
            tx.execute(
                "INSERT INTO tabs (name, uuid, created_at, updated_at, deleted, dirty)
                 VALUES (?1, ?2, ?3, ?4, ?5, 0)",
                params![
                    remote.name,
                    remote.id,
                    created,
                    remote.updated_at,
                    remote.deleted as i64
                ],
            )?;
            Ok(tx.last_insert_rowid())
        }
        Some((id, local_updated, _deleted)) => {
            // Last-write-wins. Em empate ficamos com o local: se ele for sujo,
            // ainda vai subir no push.
            if is_newer(&remote.updated_at, &local_updated) {
                tx.execute(
                    "UPDATE tabs SET name = ?2, updated_at = ?3, deleted = ?4, dirty = 0
                     WHERE id = ?1",
                    params![id, remote.name, remote.updated_at, remote.deleted as i64],
                )?;
            }
            Ok(id)
        }
    }
}

fn merge_todo(
    tx: &Transaction,
    remote: &RemoteTodo,
    local_tab: i64,
    now: &str,
) -> rusqlite::Result<()> {
    let existing: Option<(i64, String)> = tx
        .query_row(
            "SELECT id, COALESCE(updated_at, '') FROM todos WHERE uuid = ?1",
            params![remote.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;

    match existing {
        None => {
            let created = remote
                .created_at
                .clone()
                .unwrap_or_else(|| now.to_string());
            tx.execute(
                "INSERT INTO todos
                    (tab_id, text, done, completed_at, uuid, created_at, updated_at, deleted, dirty)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0)",
                params![
                    local_tab,
                    remote.text,
                    remote.done as i64,
                    remote.completed_at,
                    remote.id,
                    created,
                    remote.updated_at,
                    remote.deleted as i64
                ],
            )?;
        }
        Some((id, local_updated)) => {
            if is_newer(&remote.updated_at, &local_updated) {
                tx.execute(
                    "UPDATE todos
                     SET tab_id = ?2, text = ?3, done = ?4, completed_at = ?5,
                         updated_at = ?6, deleted = ?7, dirty = 0
                     WHERE id = ?1",
                    params![
                        id,
                        local_tab,
                        remote.text,
                        remote.done as i64,
                        remote.completed_at,
                        remote.updated_at,
                        remote.deleted as i64
                    ],
                )?;
            }
        }
    }
    Ok(())
}

/// Lê o que precisa subir e devolve já no formato do PostgREST.
fn dirty_rows(conn: &Connection) -> rusqlite::Result<(Vec<PushTab>, Vec<PushTodo>)> {
    let now = db::now_utc();

    let mut tabs = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT uuid, name, created_at, updated_at, deleted FROM tabs WHERE dirty = 1",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(PushTab {
            id: r.get(0)?,
            name: r.get(1)?,
            created_at: r.get::<_, Option<String>>(2)?.unwrap_or_else(|| now.clone()),
            updated_at: r.get::<_, Option<String>>(3)?.unwrap_or_else(|| now.clone()),
            deleted: r.get::<_, i64>(4)? != 0,
        })
    })?;
    for row in rows {
        tabs.push(row?);
    }

    let mut todos = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT t.uuid, tab.uuid, t.text, t.done, t.completed_at,
                t.created_at, t.updated_at, t.deleted
         FROM todos t
         JOIN tabs tab ON tab.id = t.tab_id
         WHERE t.dirty = 1",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(PushTodo {
            id: r.get(0)?,
            tab_id: r.get(1)?,
            text: r.get(2)?,
            done: r.get::<_, i64>(3)? != 0,
            completed_at: r.get(4)?,
            created_at: r.get::<_, Option<String>>(5)?.unwrap_or_else(|| now.clone()),
            updated_at: r.get::<_, Option<String>>(6)?.unwrap_or_else(|| now.clone()),
            deleted: r.get::<_, i64>(7)? != 0,
        })
    })?;
    for row in rows {
        todos.push(row?);
    }

    Ok((tabs, todos))
}

/// Limpa o `dirty` só de quem realmente subiu, conferindo também o `updated_at`
/// para não apagar uma alteração que aconteceu durante o envio.
fn clear_dirty(
    conn: &Connection,
    tabs: &[PushTab],
    todos: &[PushTodo],
) -> rusqlite::Result<()> {
    for tab in tabs {
        conn.execute(
            "UPDATE tabs SET dirty = 0 WHERE uuid = ?1 AND updated_at = ?2",
            params![tab.id, tab.updated_at],
        )?;
    }
    for todo in todos {
        conn.execute(
            "UPDATE todos SET dirty = 0 WHERE uuid = ?1 AND updated_at = ?2",
            params![todo.id, todo.updated_at],
        )?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Sessão local (tabela `meta`)
// ---------------------------------------------------------------------------

pub fn store_session(conn: &Connection, session: &Session) -> rusqlite::Result<()> {
    set_meta(conn, META_ACCESS, &session.access_token)?;
    set_meta(conn, META_REFRESH, &session.refresh_token)?;
    set_meta(conn, META_EXPIRES, &session.expires_at.to_string())?;
    set_meta(conn, META_USER_ID, &session.user_id)?;
    set_meta(conn, META_EMAIL, session.email.as_deref().unwrap_or(""))?;
    Ok(())
}

pub fn load_session(conn: &Connection) -> rusqlite::Result<Option<Session>> {
    let (Some(access_token), Some(refresh_token), Some(expires_at), Some(user_id)) = (
        get_meta(conn, META_ACCESS)?,
        get_meta(conn, META_REFRESH)?,
        get_meta(conn, META_EXPIRES)?,
        get_meta(conn, META_USER_ID)?,
    ) else {
        return Ok(None);
    };

    Ok(Some(Session {
        access_token,
        refresh_token,
        expires_at: expires_at.parse().unwrap_or(0),
        user_id,
        email: get_meta(conn, META_EMAIL)?.filter(|e| !e.is_empty()),
    }))
}

/// Monta os dados de Realtime do usuário conectado, ou `None` se não há sessão
/// (ou se o sync não está configurado). A senha nunca entra aqui.
pub fn realtime_config(conn: &Connection) -> Result<Option<RealtimeConfig>, String> {
    let Some(cfg) = config() else {
        return Ok(None);
    };
    let Some(session) = load_session(conn).map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    Ok(Some(RealtimeConfig {
        url: cfg.url,
        anon: cfg.anon,
        access_token: session.access_token,
        user_id: session.user_id,
    }))
}

pub fn clear_session(conn: &Connection) -> rusqlite::Result<()> {
    for key in [
        META_ACCESS,
        META_REFRESH,
        META_EXPIRES,
        META_USER_ID,
        META_EMAIL,
        META_LAST_SYNC,
    ] {
        conn.execute("DELETE FROM meta WHERE key = ?1", params![key])?;
    }
    Ok(())
}

/// Esquece a conta **e apaga a cópia local**. O banco local pertence a quem está
/// logado: se as linhas de uma conta antiga ficassem, no login de outra conta o
/// primeiro sync tentaria subir os mesmos `uuid` e o upsert bateria na linha do
/// dono original, que o RLS recusa (a outra conta não pode alterá-la). Os dados
/// continuam no servidor e voltam no próximo login da conta original.
pub fn forget_account(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch("DELETE FROM todos; DELETE FROM tabs;")?;
    conn.execute("DELETE FROM meta WHERE key = 'active_id'", [])?;
    clear_session(conn)
}

fn set_meta(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

fn get_meta(conn: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| {
        r.get(0)
    })
    .optional()
}

// ---------------------------------------------------------------------------
// Orquestração
// ---------------------------------------------------------------------------

/// Login + persistência da sessão. Separado de [`sign_in`] para o comando Tauri
/// não segurar o lock do banco durante a chamada de rede.
pub async fn sign_in_and_store(
    cfg: &Config,
    db: &Mutex<Connection>,
    email: &str,
    password: &str,
) -> Result<AuthInfo, String> {
    let session = sign_in(cfg, email, password).await?;
    {
        let conn = db.lock().map_err(|e| e.to_string())?;
        store_session(&conn, &session).map_err(|e| e.to_string())?;
    }
    Ok(session.into())
}

/// Um ciclo completo: pull → merge → push. O lock do banco fica preso apenas em
/// pequenos trechos, nunca durante as chamadas de rede.
pub async fn sync(cfg: &Config, db: &Mutex<Connection>) -> Result<SyncReport, String> {
    let mut session = {
        let conn = db.lock().map_err(|e| e.to_string())?;
        load_session(&conn).map_err(|e| e.to_string())?
    }
    .ok_or_else(|| "não autenticado".to_string())?;

    if is_expiring(&session) {
        session = refresh(cfg, &session.refresh_token).await?;
        let conn = db.lock().map_err(|e| e.to_string())?;
        store_session(&conn, &session).map_err(|e| e.to_string())?;
    }

    // Primeiro sync deste banco: tudo que existe localmente ainda não foi pro
    // servidor (mesmo que o `dirty` tenha sido zerado por uma migração antiga),
    // então força a subida. Como o pull+merge vem antes do push, o que for mais
    // novo no servidor já terá vencido localmente e não será sobrescrito.
    let first_sync = {
        let conn = db.lock().map_err(|e| e.to_string())?;
        get_meta(&conn, META_LAST_SYNC)
            .map_err(|e| e.to_string())?
            .is_none()
    };
    if first_sync {
        let conn = db.lock().map_err(|e| e.to_string())?;
        conn.execute_batch("UPDATE tabs SET dirty = 1; UPDATE todos SET dirty = 1;")
            .map_err(|e| e.to_string())?;
    }

    let remote = pull(cfg, &session).await?;
    let pulled_tabs = remote.tabs.len();
    let pulled_todos = remote.todos.len();

    let (tabs, todos) = {
        let mut conn = db.lock().map_err(|e| e.to_string())?;
        merge(&mut conn, &remote).map_err(|e| e.to_string())?;
        dirty_rows(&conn).map_err(|e| e.to_string())?
    };

    if !tabs.is_empty() {
        push_rows(cfg, &session, "tabs", &tabs).await?;
    }
    if !todos.is_empty() {
        push_rows(cfg, &session, "todos", &todos).await?;
    }

    {
        let conn = db.lock().map_err(|e| e.to_string())?;
        clear_dirty(&conn, &tabs, &todos).map_err(|e| e.to_string())?;
        set_meta(&conn, META_LAST_SYNC, &db::now_utc()).map_err(|e| e.to_string())?;
    }

    Ok(SyncReport {
        pulled_tabs,
        pulled_todos,
        pushed_tabs: tabs.len(),
        pushed_todos: todos.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{self, IncomingStore, IncomingTab, IncomingTodo};

    fn tab(id: &str, name: &str, updated: &str, deleted: bool) -> RemoteTab {
        RemoteTab {
            id: id.to_string(),
            name: name.to_string(),
            created_at: Some(updated.to_string()),
            updated_at: updated.to_string(),
            deleted,
        }
    }

    fn todo(id: &str, tab_id: &str, text: &str, updated: &str, deleted: bool) -> RemoteTodo {
        RemoteTodo {
            id: id.to_string(),
            tab_id: tab_id.to_string(),
            text: text.to_string(),
            done: false,
            completed_at: None,
            created_at: Some(updated.to_string()),
            updated_at: updated.to_string(),
            deleted,
        }
    }

    fn remote(tabs: Vec<RemoteTab>, todos: Vec<RemoteTodo>) -> RemoteData {
        RemoteData { tabs, todos }
    }

    fn local_store() -> IncomingStore {
        IncomingStore {
            tabs: vec![IncomingTab {
                id: 1,
                name: "Local".into(),
                todos: vec![IncomingTodo {
                    id: 1,
                    text: "local".into(),
                    done: false,
                }],
            }],
            active_id: 1,
        }
    }

    #[test]
    fn merge_insere_linhas_novas() {
        let mut conn = db::memory();
        merge(
            &mut conn,
            &remote(
                vec![tab("t1", "Tarefas", "2026-01-01T00:00:00.000Z", false)],
                vec![todo(
                    "x1",
                    "t1",
                    "comprar pão",
                    "2026-01-01T00:00:00.000Z",
                    false,
                )],
            ),
        )
        .unwrap();

        let estado = db::load_state(&conn).unwrap();
        assert_eq!(estado.tabs.len(), 1);
        assert_eq!(estado.tabs[0].name, "Tarefas");
        assert_eq!(estado.tabs[0].todos.len(), 1);
        assert_eq!(estado.tabs[0].todos[0].text, "comprar pão");
    }

    #[test]
    fn merge_respeita_last_write_wins() {
        let mut conn = db::memory();
        db::save_state(&mut conn, local_store()).unwrap();

        let tab_uuid: String = conn
            .query_row("SELECT uuid FROM tabs WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        let todo_uuid: String = conn
            .query_row("SELECT uuid FROM todos WHERE id = 1", [], |r| r.get(0))
            .unwrap();

        // Remoto mais antigo: o local vence e continua sujo pra subir.
        merge(
            &mut conn,
            &remote(
                vec![tab(&tab_uuid, "Remoto antigo", "2000-01-01T00:00:00.000Z", false)],
                vec![todo(
                    &todo_uuid,
                    &tab_uuid,
                    "remoto antigo",
                    "2000-01-01T00:00:00.000Z",
                    false,
                )],
            ),
        )
        .unwrap();
        let estado = db::load_state(&conn).unwrap();
        assert_eq!(estado.tabs[0].name, "Local");
        assert_eq!(estado.tabs[0].todos[0].text, "local");

        // Remoto mais novo: sobrescreve e limpa o dirty.
        merge(
            &mut conn,
            &remote(
                vec![tab(&tab_uuid, "Remoto novo", "2999-01-01T00:00:00.000Z", false)],
                vec![todo(
                    &todo_uuid,
                    &tab_uuid,
                    "remoto novo",
                    "2999-01-01T00:00:00.000Z",
                    false,
                )],
            ),
        )
        .unwrap();
        let estado = db::load_state(&conn).unwrap();
        assert_eq!(estado.tabs[0].name, "Remoto novo");
        assert_eq!(estado.tabs[0].todos[0].text, "remoto novo");

        let dirty: i64 = conn
            .query_row("SELECT COUNT(*) FROM tabs WHERE dirty = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(dirty, 0);
    }

    #[test]
    fn merge_aplica_tombstone() {
        let mut conn = db::memory();
        merge(
            &mut conn,
            &remote(
                vec![tab("t1", "Tarefas", "2026-01-01T00:00:00.000Z", false)],
                vec![todo("x1", "t1", "some", "2026-01-01T00:00:00.000Z", false)],
            ),
        )
        .unwrap();

        merge(
            &mut conn,
            &remote(
                vec![tab("t1", "Tarefas", "2026-01-01T00:00:00.000Z", false)],
                vec![todo("x1", "t1", "some", "2026-02-01T00:00:00.000Z", true)],
            ),
        )
        .unwrap();

        let estado = db::load_state(&conn).unwrap();
        assert_eq!(estado.tabs[0].todos.len(), 0, "o TODO apagado some da visão");
    }

    #[test]
    fn dirty_rows_so_pega_o_que_mudou() {
        let mut conn = db::memory();
        db::save_state(&mut conn, local_store()).unwrap();

        let (tabs, todos) = dirty_rows(&conn).unwrap();
        assert_eq!(tabs.len(), 1);
        assert_eq!(todos.len(), 1);

        clear_dirty(&conn, &tabs, &todos).unwrap();
        let (tabs, todos) = dirty_rows(&conn).unwrap();
        assert!(tabs.is_empty());
        assert!(todos.is_empty());
    }

    #[test]
    fn is_newer_compara_z_e_offset() {
        // O Postgres devolve `+00:00` onde o SQLite grava `Z`.
        assert!(is_newer(
            "2026-01-01T00:00:01.000+00:00",
            "2026-01-01T00:00:00.000Z"
        ));
        assert!(!is_newer(
            "2026-01-01T00:00:00.000+00:00",
            "2026-01-01T00:00:01.000Z"
        ));
        // Mesmo instante em formatos diferentes não conta como mais novo.
        assert!(!is_newer(
            "2026-01-01T00:00:00.123+00:00",
            "2026-01-01T00:00:00.123Z"
        ));
    }

    #[test]
    fn forget_account_apaga_dados_e_sessao() {
        let mut conn = db::memory();
        db::save_state(&mut conn, local_store()).unwrap();

        let session = Session {
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_at: 0,
            user_id: "u".into(),
            email: None,
        };
        store_session(&conn, &session).unwrap();

        forget_account(&conn).unwrap();

        let estado = db::load_state(&conn).unwrap();
        assert!(estado.tabs.is_empty(), "as abas locais somem");
        assert!(load_session(&conn).unwrap().is_none(), "a sessão some");
        assert_eq!(db::pending_count(&conn).unwrap(), 0);
    }
}
