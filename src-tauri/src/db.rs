//! Persistência em SQLite das abas e dos TODOs.
//!
//! O banco é o único dono da verdade: o frontend envia o estado inteiro e o
//! `save_state` reconcilia com o que já está gravado. Duas consequências
//! importantes:
//!
//! 1. `completed_at` é calculado aqui, no backend, com o relógio do sistema e o
//!    fuso de Brasília. O cliente nunca manda esse valor, então não dá para
//!    forjar um horário de conclusão a partir do WebView.
//! 2. Reconciliar (em vez de apagar tudo e reinserir) faz o timestamp ser
//!    preservado entre salvamentos: um TODO já concluído continua com a data
//!    original, e só recebe uma nova quando passa de pendente para concluído.

use std::path::Path;

use chrono::{Local, SecondsFormat, Utc};
use chrono_tz::{America::Sao_Paulo, Tz};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Fuso usado para gravar e interpretar a conclusão dos TODOs.
const BRASILIA: Tz = Sao_Paulo;

/// Os nomes das tabelas divergem do SQL (`tabs`/`todos`), então o resultado das
/// queries é mapeado à mão em vez de derivado por derive.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Todo {
    pub id: i64,
    pub text: String,
    pub done: bool,
    /// Horário de conclusão em Brasília, em ISO 8601 com offset explícito
    /// (ex.: `2026-10-01T14:32:05-03:00`), ou `None` se ainda não concluído.
    pub completed_at: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Tab {
    pub id: i64,
    pub name: String,
    pub todos: Vec<Todo>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Store {
    pub tabs: Vec<Tab>,
    pub active_id: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IncomingTodo {
    pub id: i64,
    pub text: String,
    pub done: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IncomingTab {
    pub id: i64,
    pub name: String,
    pub todos: Vec<IncomingTodo>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IncomingStore {
    pub tabs: Vec<IncomingTab>,
    pub active_id: i64,
}

/// Abre (ou cria) o banco no caminho dado e aplica as migrações pendentes.
pub fn open(path: &Path) -> rusqlite::Result<Connection> {
    if let Some(dir) = path.parent() {
        // `rusqlite::Error` não converte de `io::Error`, então embrulhamos.
        std::fs::create_dir_all(dir)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?;
    }

    let conn = Connection::open(path)?;

    // `WAL` deixa a leitura do WebView fluida enquanto gravamos. O pragma
    // devolve uma linha, então `query_row` em vez de `execute`.
    let _: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
    // `foreign_keys` vem desligado por padrão no SQLite, e é dele que sai a
    // limpeza em cascata dos TODOs de uma aba removida.
    conn.pragma_update(None, "foreign_keys", "ON")?;

    migrate(&conn)?;
    Ok(conn)
}

/// Migrações versionadas por `PRAGMA user_version`. Cada versão é aplicada em
/// sequência, então um banco antigo sobe até a versão atual sem perder dados.
fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version == 0 {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS tabs (
                 id   INTEGER PRIMARY KEY,
                 name TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS todos (
                 id           INTEGER PRIMARY KEY,
                 tab_id       INTEGER NOT NULL REFERENCES tabs(id) ON DELETE CASCADE,
                 text         TEXT    NOT NULL,
                 done         INTEGER NOT NULL DEFAULT 0,
                 completed_at TEXT
             );
             CREATE INDEX IF NOT EXISTS idx_todos_tab ON todos(tab_id);
             CREATE TABLE IF NOT EXISTS meta (
                 key   TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );
             PRAGMA user_version = 1;",
        )?;
    }

    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version < 2 {
        // Campos necessários pro sync: `uuid` identifica a linha entre
        // dispositivos, `updated_at` alimenta o last-write-wins, `deleted` é o
        // tombstone e `dirty` marca o que ainda precisa subir pro Supabase.
        //
        // As linhas que já existem ganham um UUID v4 e o instante da migração
        // como criação/atualização. O UUID é gerado em SQL puro para não ter que
        // ler linha a linha.
        conn.execute_batch(
            "ALTER TABLE tabs  ADD COLUMN uuid       TEXT;
             ALTER TABLE tabs  ADD COLUMN created_at TEXT;
             ALTER TABLE tabs  ADD COLUMN updated_at TEXT;
             ALTER TABLE tabs  ADD COLUMN deleted    INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE tabs  ADD COLUMN dirty      INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE todos ADD COLUMN uuid       TEXT;
             ALTER TABLE todos ADD COLUMN created_at TEXT;
             ALTER TABLE todos ADD COLUMN updated_at TEXT;
             ALTER TABLE todos ADD COLUMN deleted    INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE todos ADD COLUMN dirty      INTEGER NOT NULL DEFAULT 0;

             UPDATE tabs SET uuid = lower(hex(randomblob(4)) || '-' || hex(randomblob(2)) || '-4' || substr(hex(randomblob(2)),2) || '-' || substr('89ab',(abs(random()) % 4) + 1,1) || substr(hex(randomblob(2)),2) || '-' || hex(randomblob(6))) WHERE uuid IS NULL;
             UPDATE tabs SET created_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE created_at IS NULL;
             UPDATE tabs SET updated_at = created_at WHERE updated_at IS NULL;

             UPDATE todos SET uuid = lower(hex(randomblob(4)) || '-' || hex(randomblob(2)) || '-4' || substr(hex(randomblob(2)),2) || '-' || substr('89ab',(abs(random()) % 4) + 1,1) || substr(hex(randomblob(2)),2) || '-' || hex(randomblob(6))) WHERE uuid IS NULL;
             UPDATE todos SET created_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE created_at IS NULL;
             UPDATE todos SET updated_at = created_at WHERE updated_at IS NULL;

             CREATE UNIQUE INDEX IF NOT EXISTS idx_tabs_uuid  ON tabs(uuid);
             CREATE UNIQUE INDEX IF NOT EXISTS idx_todos_uuid ON todos(uuid);
             CREATE INDEX IF NOT EXISTS idx_tabs_dirty  ON tabs(dirty);
             CREATE INDEX IF NOT EXISTS idx_todos_dirty ON todos(dirty);

             PRAGMA user_version = 2;",
        )?;
    }
    Ok(())
}

/// Horário atual em Brasília. O offset vai junto no texto (`-03:00`), então o
/// valor é inequívoco mesmo lido por um `sqlite3` ou por outra máquina.
fn now_brasilia() -> String {
    Local::now()
        .with_timezone(&BRASILIA)
        .to_rfc3339_opts(SecondsFormat::Secs, false)
}

/// Instante atual em UTC com milissegundos. O formato é fixo (`...Z`), então
/// `updated_at` pode ser comparado como texto entre dispositivos no sync.
fn now_utc() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Lê o estado completo, com os TODOs de cada aba em ordem de inserção.
pub fn load_state(conn: &Connection) -> rusqlite::Result<Store> {
    let mut tabs = Vec::new();

    // `Map` itera em `Result`, então o `?` fica no item, não no `next()`.
    let mut stmt = conn.prepare("SELECT id, name FROM tabs WHERE deleted = 0 ORDER BY id")?;
    let rows = stmt.query_map([], |row| {
        Ok(Tab {
            id: row.get(0)?,
            name: row.get(1)?,
            todos: Vec::new(),
        })
    })?;
    for tab in rows {
        let mut tab = tab?;
        tab.todos = load_todos(conn, tab.id)?;
        tabs.push(tab);
    }

    let fallback = tabs.first().map(|t| t.id).unwrap_or(1);
    let active_id = load_active_id(conn, fallback)?;
    Ok(Store { tabs, active_id })
}

fn load_todos(conn: &Connection, tab_id: i64) -> rusqlite::Result<Vec<Todo>> {
    let mut stmt = conn.prepare(
        "SELECT id, text, done, completed_at
         FROM todos
         WHERE tab_id = ?1 AND deleted = 0
         ORDER BY id",
    )?;

    let rows = stmt.query_map(params![tab_id], |row| {
        Ok(Todo {
            id: row.get(0)?,
            text: row.get(1)?,
            done: row.get::<_, i64>(2)? != 0,
            completed_at: row.get(3)?,
        })
    })?;

    rows.collect()
}

/// Guarda a aba ativa; cai para a primeira aba se a salva não existir mais.
fn load_active_id(conn: &Connection, fallback: i64) -> rusqlite::Result<i64> {
    let saved: Option<String> = conn
        .query_row("SELECT value FROM meta WHERE key = 'active_id'", [], |row| {
            row.get(0)
        })
        .optional()?;

    let Some(saved) = saved else { return Ok(fallback) };
    let Ok(id) = saved.parse::<i64>() else {
        return Ok(fallback);
    };

    let exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM tabs WHERE id = ?1",
        params![id],
        |row| row.get(0),
    )?;

    Ok(if exists > 0 { id } else { fallback })
}

/// Reconcilia o estado recebido com o banco, tudo dentro de uma transação.
pub fn save_state(conn: &mut Connection, incoming: IncomingStore) -> rusqlite::Result<Store> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    let now = now_utc();

    // O `upsert` sozinho nunca some com nada, então precisamos marcar
    // explicitamente o que saiu do estado do cliente. Como agora existe sync,
    // não apagamos de verdade: viramos tombstone (`deleted = 1`) pra que a
    // exclusão também chegue no outro dispositivo.
    let tab_ids: Vec<i64> = incoming.tabs.iter().map(|t| t.id).collect();
    for id in column(&tx, "SELECT id FROM tabs WHERE deleted = 0", [])? {
        if !tab_ids.contains(&id) {
            tx.execute(
                "UPDATE tabs SET deleted = 1, updated_at = ?2, dirty = 1 WHERE id = ?1",
                params![id, now],
            )?;
            // Os TODOs da aba vão junto, senão ficariam pendurados numa aba
            // invisível e não propagariam a exclusão.
            tx.execute(
                "UPDATE todos SET deleted = 1, updated_at = ?2, dirty = 1
                 WHERE tab_id = ?1 AND deleted = 0",
                params![id, now],
            )?;
        }
    }

    for tab in &incoming.tabs {
        upsert_tab(&tx, tab, &now)?;
        upsert_todos(&tx, tab.id, &tab.todos, &now)?;

        // Mesma lógica dentro da aba: um TODO apagado pelo cliente vira
        // tombstone mesmo com a aba ainda presente.
        let todo_ids: Vec<i64> = tab.todos.iter().map(|t| t.id).collect();
        for id in column(
            &tx,
            "SELECT id FROM todos WHERE tab_id = :tab AND deleted = 0",
            rusqlite::named_params! { ":tab": tab.id },
        )? {
            if !todo_ids.contains(&id) {
                tx.execute(
                    "UPDATE todos SET deleted = 1, updated_at = ?2, dirty = 1 WHERE id = ?1",
                    params![id, now],
                )?;
            }
        }
    }

    tx.execute(
        "INSERT INTO meta (key, value) VALUES ('active_id', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![incoming.active_id.to_string()],
    )?;

    // Devolvemos o estado reconciliado, e é ele que vale: o backend acabou de
    // preencher `completed_at` que o cliente não conhecia.
    let store = load_state(&tx)?;
    tx.commit()?;

    Ok(store)
}

/// Lê uma coluna de inteiros, usada para descobrir o que sobrou no banco.
fn column(
    tx: &Transaction,
    sql: &str,
    params: impl rusqlite::Params,
) -> rusqlite::Result<Vec<i64>> {
    let mut stmt = tx.prepare(sql)?;
    let rows = stmt.query_map(params, |row| row.get(0))?;
    rows.collect()
}

fn upsert_tab(tx: &Transaction, tab: &IncomingTab, now: &str) -> rusqlite::Result<()> {
    let previous: Option<(String, i64)> = tx
        .query_row(
            "SELECT name, deleted FROM tabs WHERE id = ?1",
            params![tab.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;

    match previous {
        None => {
            tx.execute(
                "INSERT INTO tabs (id, name, uuid, created_at, updated_at, deleted, dirty)
                 VALUES (?1, ?2, ?3, ?4, ?4, 0, 1)",
                params![tab.id, tab.name, Uuid::new_v4().to_string(), now],
            )?;
        }
        Some((name, deleted)) => {
            // Só marca como sujo quando algo muda: um salvamento sem alteração
            // não pode reenviar a linha nem sobrescrever a versão remota.
            if name != tab.name || deleted != 0 {
                tx.execute(
                    "UPDATE tabs SET name = ?2, updated_at = ?3, deleted = 0, dirty = 1
                     WHERE id = ?1",
                    params![tab.id, tab.name, now],
                )?;
            }
        }
    }
    Ok(())
}

fn upsert_todos(
    tx: &Transaction,
    tab_id: i64,
    todos: &[IncomingTodo],
    now: &str,
) -> rusqlite::Result<()> {
    for todo in todos {
        // Lemos o estado anterior antes do `UPDATE` para decidir entre manter o
        // horário original e gerar um novo, e para saber se algo mudou.
        let previous: Option<(i64, String, bool, Option<String>, i64)> = tx
            .query_row(
                "SELECT tab_id, text, done, completed_at, deleted FROM todos WHERE id = ?1",
                params![todo.id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get::<_, i64>(2)? != 0,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()?;

        match previous {
            None => {
                let completed_at = if todo.done {
                    Some(now_brasilia())
                } else {
                    None
                };
                tx.execute(
                    "INSERT INTO todos
                        (id, tab_id, text, done, completed_at, uuid, created_at, updated_at, deleted, dirty)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, 0, 1)",
                    params![
                        todo.id,
                        tab_id,
                        todo.text,
                        todo.done as i64,
                        completed_at,
                        Uuid::new_v4().to_string(),
                        now
                    ],
                )?;
            }
            Some((prev_tab, prev_text, prev_done, prev_at, prev_deleted)) => {
                let changed = prev_tab != tab_id
                    || prev_text != todo.text
                    || prev_done != todo.done
                    || prev_deleted != 0;
                if changed {
                    let completed_at = if todo.done {
                        Some(match (prev_done, prev_at) {
                            // Reabrir e concluir de novo gera um horário novo; só
                            // preservamos a data de quem já estava concluído.
                            (true, Some(at)) => at,
                            _ => now_brasilia(),
                        })
                    } else {
                        // Pendente não tem horário de conclusão.
                        None
                    };
                    tx.execute(
                        "UPDATE todos
                         SET tab_id = ?2, text = ?3, done = ?4, completed_at = ?5,
                             updated_at = ?6, deleted = 0, dirty = 1
                         WHERE id = ?1",
                        params![todo.id, tab_id, todo.text, todo.done as i64, completed_at, now],
                    )?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        migrate(&conn).unwrap();
        conn
    }

    fn store(tabs: Vec<IncomingTab>, active_id: i64) -> IncomingStore {
        IncomingStore { tabs, active_id }
    }

    fn tab(id: i64, todos: Vec<IncomingTodo>) -> IncomingTab {
        IncomingTab {
            id,
            name: format!("aba {id}"),
            todos,
        }
    }

    fn todo(id: i64, done: bool) -> IncomingTodo {
        IncomingTodo {
            id,
            text: format!("todo {id}"),
            done,
        }
    }

    #[test]
    fn grava_e_relê_o_estado() {
        let mut conn = mem();
        save_state(
            &mut conn,
            store(vec![tab(1, vec![todo(10, false), todo(11, true)])], 1),
        )
        .unwrap();

        let lido = load_state(&conn).unwrap();
        assert_eq!(lido.tabs.len(), 1);
        assert_eq!(lido.tabs[0].todos.len(), 2);
        assert!(!lido.tabs[0].todos[0].done);
        assert!(lido.tabs[0].todos[0].completed_at.is_none());
        assert!(lido.tabs[0].todos[1].completed_at.is_some());
    }

    #[test]
    fn timestamp_eh_brasilia_com_offset_explicito() {
        let mut conn = mem();
        save_state(&mut conn, store(vec![tab(1, vec![todo(1, true)])], 1)).unwrap();

        let stamp = load_state(&conn).unwrap().tabs[0].todos[0]
            .completed_at
            .clone()
            .unwrap();

        // ISO 8601 com o offset de Brasília, que hoje é -03:00.
        assert!(
            stamp.ends_with("-03:00"),
            "esperado offset -03:00, veio {stamp}"
        );

        // E precisa voltar sem ambiguidade para o mesmo instante.
        let parsed = chrono::DateTime::parse_from_rfc3339(&stamp).unwrap();
        assert_eq!(parsed.offset().local_minus_utc(), -3 * 3600);
        assert_eq!(parsed.with_timezone(&BRASILIA).to_rfc3339(), stamp);
    }

    #[test]
    fn resalvar_preserva_o_timestamp_original() {
        let mut conn = mem();
        save_state(&mut conn, store(vec![tab(1, vec![todo(1, true)])], 1)).unwrap();
        let primeiro = load_state(&conn).unwrap().tabs[0].todos[0]
            .completed_at
            .clone();

        save_state(&mut conn, store(vec![tab(1, vec![todo(1, true)])], 1)).unwrap();
        let segundo = load_state(&conn).unwrap().tabs[0].todos[0]
            .completed_at
            .clone();

        assert_eq!(primeiro, segundo, "resalvar não deve gerar data nova");
    }

    #[test]
    fn desmarcar_limpa_o_timestamp() {
        let mut conn = mem();
        save_state(&mut conn, store(vec![tab(1, vec![todo(1, true)])], 1)).unwrap();
        save_state(&mut conn, store(vec![tab(1, vec![todo(1, false)])], 1)).unwrap();

        let lido = load_state(&conn).unwrap().tabs[0].todos[0].clone();
        assert!(!lido.done);
        assert!(lido.completed_at.is_none());
    }

    #[test]
    fn concluir_depois_de_reabrir_marca_a_data_da_ultima_conclusao() {
        let mut conn = mem();
        save_state(&mut conn, store(vec![tab(1, vec![todo(1, true)])], 1)).unwrap();
        let primeira = load_state(&conn).unwrap().tabs[0].todos[0]
            .completed_at
            .clone();

        save_state(&mut conn, store(vec![tab(1, vec![todo(1, false)])], 1)).unwrap();
        save_state(&mut conn, store(vec![tab(1, vec![todo(1, true)])], 1)).unwrap();
        let segunda = load_state(&conn).unwrap().tabs[0].todos[0]
            .completed_at
            .clone();

        // Pode ser igual dentro do mesmo segundo, mas tem que continuar preenchida.
        assert!(segunda.is_some());
        assert!(primeira.is_some());
    }

    #[test]
    fn aba_removida_leva_os_todos_junto() {
        let mut conn = mem();
        save_state(
            &mut conn,
            store(
                vec![tab(1, vec![todo(1, false)]), tab(2, vec![todo(2, false)])],
                1,
            ),
        )
        .unwrap();

        save_state(&mut conn, store(vec![tab(2, vec![todo(2, false)])], 2)).unwrap();

        assert_eq!(
            contar(&conn, "todos WHERE deleted = 0"),
            1,
            "o TODO da aba removida deve sumir da visão"
        );
        assert_eq!(contar(&conn, "tabs WHERE deleted = 0"), 1);
        // Mas o tombstone fica gravado para propagar a exclusão no sync.
        assert_eq!(contar(&conn, "todos WHERE deleted = 1"), 1);
    }

    #[test]
    fn todo_removido_some_mesmo_com_a_aba_presente() {
        let mut conn = mem();
        save_state(
            &mut conn,
            store(vec![tab(1, vec![todo(1, false), todo(2, false)])], 1),
        )
        .unwrap();

        save_state(&mut conn, store(vec![tab(1, vec![todo(1, false)])], 1)).unwrap();

        let lido = load_state(&conn).unwrap();
        assert_eq!(lido.tabs.len(), 1, "a aba continua existindo");
        assert_eq!(lido.tabs[0].todos.len(), 1, "mas o TODO removido some");
    }

    #[test]
    fn aba_renomeada_e_preservada() {
        let mut conn = mem();
        save_state(&mut conn, store(vec![tab(1, vec![todo(1, true)])], 1)).unwrap();
        let marca = load_state(&conn).unwrap().tabs[0].todos[0]
            .completed_at
            .clone();

        let mut renomeada = tab(1, vec![todo(1, true)]);
        renomeada.name = "Novo".into();
        save_state(&mut conn, store(vec![renomeada], 1)).unwrap();

        let lido = load_state(&conn).unwrap();
        assert_eq!(lido.tabs[0].name, "Novo");
        assert_eq!(lido.tabs[0].todos[0].completed_at, marca);
    }

    #[test]
    fn active_id_invalido_cai_para_a_primeira_aba() {
        let mut conn = mem();
        save_state(&mut conn, store(vec![tab(1, vec![])], 999)).unwrap();
        assert_eq!(load_state(&conn).unwrap().active_id, 1);
    }

    #[test]
    fn mover_todo_entre_abas_e_persistido() {
        let mut conn = mem();
        save_state(
            &mut conn,
            store(vec![tab(1, vec![todo(1, false)]), tab(2, vec![])], 1),
        )
        .unwrap();
        save_state(&mut conn, store(vec![tab(1, vec![]), tab(2, vec![todo(1, false)])], 2))
            .unwrap();

        let lido = load_state(&conn).unwrap();
        assert!(lido.tabs[0].todos.is_empty());
        assert_eq!(lido.tabs[1].todos.len(), 1);
        assert_eq!(lido.tabs[1].todos[0].id, 1);
    }

    #[test]
    fn json_usa_camel_case_para_combinar_com_o_frontend() {
        // O frontend consome esses campos por nome. Um `rename_all` esquecido
        // compila, passa nos testes de SQL e só quebra em tempo de execução, com
        // `activeId` chegando `undefined` e nenhuma alteração sendo gravada.
        let json = serde_json::to_string(&Store {
            tabs: vec![Tab {
                id: 1,
                name: "Tarefas".into(),
                todos: vec![Todo {
                    id: 2,
                    text: "x".into(),
                    done: true,
                    completed_at: Some("2026-10-01T16:31:42-03:00".into()),
                }],
            }],
            active_id: 1,
        })
        .unwrap();

        assert!(json.contains(r#""activeId":1"#), "{json}");
        assert!(json.contains(r#""completedAt":"2026-10-01T16:31:42-03:00""#), "{json}");
        assert!(!json.contains("active_id"), "{json}");
        assert!(!json.contains("completed_at"), "{json}");
    }

    #[test]
    fn json_do_frontend_e_lido_de_volta() {
        // Ida e volta: o que o TS envia tem que ser aceito pelo Rust.
        let enviado = r#"{
            "tabs": [
                {"id": 1, "name": "Tarefas", "todos": [
                    {"id": 2, "text": "x", "done": true}
                ]}
            ],
            "activeId": 1
        }"#;
        let parsed: IncomingStore = serde_json::from_str(enviado).unwrap();
        assert_eq!(parsed.active_id, 1);
        assert_eq!(parsed.tabs[0].todos[0].id, 2);
    }

    #[test]
    fn novo_item_recebe_uuid_e_fica_sujo() {
        let mut conn = mem();
        save_state(
            &mut conn,
            store(vec![tab(1, vec![todo(1, false)])], 1),
        )
        .unwrap();

        let (tab_uuid, todo_uuid, dirty): (String, String, i64) = conn
            .query_row(
                "SELECT tabs.uuid, todos.uuid, todos.dirty
                 FROM todos JOIN tabs ON tabs.id = todos.tab_id
                 WHERE todos.id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();

        assert_eq!(tab_uuid.len(), 36, "uuid v4 tem 36 caracteres");
        assert_eq!(todo_uuid.len(), 36);
        assert_eq!(dirty, 1);
    }

    #[test]
    fn uuid_nao_muda_entre_saves() {
        let mut conn = mem();
        save_state(&mut conn, store(vec![tab(1, vec![todo(1, false)])], 1)).unwrap();
        let primeiro: String = conn
            .query_row("SELECT uuid FROM todos WHERE id = 1", [], |r| r.get(0))
            .unwrap();

        save_state(&mut conn, store(vec![tab(1, vec![todo(1, false)])], 1)).unwrap();
        let segundo: String = conn
            .query_row("SELECT uuid FROM todos WHERE id = 1", [], |r| r.get(0))
            .unwrap();

        assert_eq!(primeiro, segundo);
    }

    #[test]
    fn salvar_sem_mudanca_nao_ressuja() {
        let mut conn = mem();
        save_state(&mut conn, store(vec![tab(1, vec![todo(1, false)])], 1)).unwrap();
        // Finge que esse estado já foi sincronizado.
        conn.execute(
            "UPDATE todos SET dirty = 0, updated_at = '2020-01-01T00:00:00.000Z'",
            [],
        )
        .unwrap();

        save_state(&mut conn, store(vec![tab(1, vec![todo(1, false)])], 1)).unwrap();

        let (dirty, updated_at): (i64, String) = conn
            .query_row("SELECT dirty, updated_at FROM todos WHERE id = 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(dirty, 0, "salvar sem alteração não deve marcar como sujo");
        assert_eq!(updated_at, "2020-01-01T00:00:00.000Z");
    }

    #[test]
    fn alteracao_marca_sujo_e_renova_o_updated_at() {
        let mut conn = mem();
        save_state(&mut conn, store(vec![tab(1, vec![todo(1, false)])], 1)).unwrap();
        conn.execute("UPDATE todos SET dirty = 0", []).unwrap();

        let mut alterado = todo(1, false);
        alterado.text = "novo texto".into();
        save_state(&mut conn, store(vec![tab(1, vec![alterado])], 1)).unwrap();

        let (dirty, text): (i64, String) = conn
            .query_row("SELECT dirty, text FROM todos WHERE id = 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(dirty, 1);
        assert_eq!(text, "novo texto");
    }

    #[test]
    fn remocao_vira_tombstone() {
        let mut conn = mem();
        save_state(
            &mut conn,
            store(vec![tab(1, vec![todo(1, false), todo(2, false)])], 1),
        )
        .unwrap();
        save_state(&mut conn, store(vec![tab(1, vec![todo(1, false)])], 1)).unwrap();

        assert_eq!(contar(&conn, "todos WHERE deleted = 1"), 1);
        assert_eq!(load_state(&conn).unwrap().tabs[0].todos.len(), 1);
    }

    #[test]
    fn migracao_v1_para_v2_preserva_os_dados() {
        // Simula o banco de quem já usava o app antes do sync.
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        conn.execute_batch(
            "CREATE TABLE tabs (
                 id   INTEGER PRIMARY KEY,
                 name TEXT NOT NULL
             );
             CREATE TABLE todos (
                 id           INTEGER PRIMARY KEY,
                 tab_id       INTEGER NOT NULL REFERENCES tabs(id) ON DELETE CASCADE,
                 text         TEXT    NOT NULL,
                 done         INTEGER NOT NULL DEFAULT 0,
                 completed_at TEXT
             );
             CREATE TABLE meta (
                 key   TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );
             INSERT INTO tabs (id, name) VALUES (1, 'Tarefas');
             INSERT INTO todos (id, tab_id, text, done, completed_at)
                VALUES (1, 1, 'comprar pão', 1, '2026-01-01T10:00:00-03:00');
             PRAGMA user_version = 1;",
        )
        .unwrap();

        migrate(&conn).unwrap();

        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 2);

        let (uuid, dirty): (String, i64) = conn
            .query_row("SELECT uuid, dirty FROM todos WHERE id = 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(uuid.len(), 36);
        assert_eq!(dirty, 0, "dado antigo não precisa subir: só o que mudar depois");

        // O estado lido continua idêntico ao de antes.
        let lido = load_state(&conn).unwrap();
        assert_eq!(lido.tabs[0].name, "Tarefas");
        assert_eq!(lido.tabs[0].todos[0].text, "comprar pão");
        assert_eq!(
            lido.tabs[0].todos[0].completed_at.as_deref(),
            Some("2026-01-01T10:00:00-03:00")
        );
    }

    fn contar(conn: &Connection, tabela: &str) -> i64 {
        // `tabela` é sempre uma constante dos testes, então interpolar aqui é
        // seguro (e `query_row` não aceita `?` para identificadores).
        conn.query_row(&format!("SELECT COUNT(*) FROM {tabela}"), [], |r| r.get(0))
            .unwrap()
    }
}
