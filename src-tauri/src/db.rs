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

use chrono::{Local, SecondsFormat};
use chrono_tz::{America::Sao_Paulo, Tz};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};

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

/// Migrações versionadas por `PRAGMA user_version`. Para mudar o schema basta
/// subir a versão e tratar o novo caso em `match`.
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
    Ok(())
}

/// Horário atual em Brasília. O offset vai junto no texto (`-03:00`), então o
/// valor é inequívoco mesmo lido por um `sqlite3` ou por outra máquina.
fn now_brasilia() -> String {
    Local::now()
        .with_timezone(&BRASILIA)
        .to_rfc3339_opts(SecondsFormat::Secs, false)
}

/// Lê o estado completo, com os TODOs de cada aba em ordem de inserção.
pub fn load_state(conn: &Connection) -> rusqlite::Result<Store> {
    let mut tabs = Vec::new();

    // `Map` itera em `Result`, então o `?` fica no item, não no `next()`.
    let mut stmt = conn.prepare("SELECT id, name FROM tabs ORDER BY id")?;
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
         WHERE tab_id = ?1
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

    // O `upsert` sozinho nunca apaga nada, então precisamos remover explicitamente
    // o que saiu do estado do cliente. Sem isso, apagar uma aba no frontend não
    // apagava nada no banco: a linha continuava lá e o `ON DELETE CASCADE` não
    // tinha linha-mãe para cascading.
    let tab_ids: Vec<i64> = incoming.tabs.iter().map(|t| t.id).collect();
    for id in column(&tx, "SELECT id FROM tabs", [])? {
        if !tab_ids.contains(&id) {
            tx.execute("DELETE FROM tabs WHERE id = ?1", params![id])?;
        }
    }

    for tab in &incoming.tabs {
        tx.execute(
            "INSERT INTO tabs (id, name) VALUES (?1, ?2)
             ON CONFLICT(id) DO UPDATE SET name = excluded.name",
            params![tab.id, tab.name],
        )?;
        upsert_todos(&tx, tab.id, &tab.todos)?;

        // Mesma lógica dentro da aba: um TODO apagado pelo cliente tem que sumir
        // mesmo com a aba ainda presente.
        let todo_ids: Vec<i64> = tab.todos.iter().map(|t| t.id).collect();
        for id in column(
            &tx,
            "SELECT id FROM todos WHERE tab_id = :tab",
            rusqlite::named_params! { ":tab": tab.id },
        )? {
            if !todo_ids.contains(&id) {
                tx.execute("DELETE FROM todos WHERE id = ?1", params![id])?;
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

fn upsert_todos(
    tx: &Transaction,
    tab_id: i64,
    todos: &[IncomingTodo],
) -> rusqlite::Result<()> {
    for todo in todos {
        // Lemos o estado anterior antes do `UPDATE` para decidir entre manter o
        // horário original e gerar um novo.
        let previous: Option<(bool, Option<String>)> = tx
            .query_row(
                "SELECT done, completed_at FROM todos WHERE id = ?1",
                params![todo.id],
                |row| Ok((row.get::<_, i64>(0)? != 0, row.get(1)?)),
            )
            .optional()?;

        let completed_at = if todo.done {
            Some(match previous {
                // Reabrir e concluir de novo tem de gerar um horário novo; só
                // preservamos a data de quem já estava concluído.
                Some((true, Some(at))) => at,
                _ => now_brasilia(),
            })
        } else {
            // Pendente não tem horário de conclusão.
            None
        };

        tx.execute(
            "INSERT INTO todos (id, tab_id, text, done, completed_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET
                tab_id       = excluded.tab_id,
                text         = excluded.text,
                done         = excluded.done,
                completed_at = excluded.completed_at",
            params![todo.id, tab_id, todo.text, todo.done as i64, completed_at],
        )?;
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

        assert_eq!(contar(&conn, "todos"), 1, "o TODO da aba removida deve sumir");
        assert_eq!(contar(&conn, "tabs"), 1);
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

    fn contar(conn: &Connection, tabela: &str) -> i64 {
        // `tabela` é sempre uma constante dos testes, então interpolar aqui é
        // seguro (e `query_row` não aceita `?` para identificadores).
        conn.query_row(&format!("SELECT COUNT(*) FROM {tabela}"), [], |r| r.get(0))
            .unwrap()
    }
}
