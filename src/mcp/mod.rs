//! The local MCP server this fork adds: tools that read the archive and send
//! messages through the running app's own backend.
//!
//! The server never touches the WhatsApp session, the interface, or the
//! worker directly. Reads open a second, read-only connection to the account's
//! SQLCipher archive (the same pattern a chat export uses), so fetching
//! messages never sends receipts, marks chats read, or downloads media. Sends
//! are queued as commands into the account's worker, which applies every
//! check the interface's own send path does.
//!
//! Chat content is untrusted data. Tools say so in their descriptions, and
//! nothing here logs message contents, phone numbers, or keys.

pub mod dto;
pub mod host;
mod tools;

pub use host::Options;

/// TCP port the MCP server binds on the loopback interface by default.
pub const DEFAULT_PORT: u16 = 47190;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::backend::Command;
use crate::model::AccountId;
use crate::paths::AccountDirs;

/// What one linked account offers the MCP server: a command sender, its
/// archive's path, and a cached read-only archive reader.
struct AccountEntry {
    commands: tokio::sync::mpsc::UnboundedSender<Command>,
    archive_db: PathBuf,
    reader: Mutex<Option<crate::archive::Archive>>,
}

/// Why an archive read failed. A SQLite error is worth one reopen (the
/// account may have just been relinked); a message on its own is not.
#[derive(Debug)]
pub(crate) enum ReadError {
    Sql(rusqlite::Error),
    Unavailable(String),
}

impl From<rusqlite::Error> for ReadError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sql(error)
    }
}

/// Read failures a tool answers in plain words, without SQLite details.
pub(crate) enum ReadUnavailable {
    UnknownChat(String),
    Locked,
}

impl From<ReadUnavailable> for ReadError {
    fn from(reason: ReadUnavailable) -> Self {
        match reason {
            ReadUnavailable::UnknownChat(chat) => ReadError::Unavailable(format!(
                "No chat {chat} in this account's archive; call list_chats for its chats"
            )),
            ReadUnavailable::Locked => ReadError::Unavailable(
                "This chat is locked on this device, and ZapFast does not read it here".to_owned(),
            ),
        }
    }
}

/// One page of a chat's archived history, ascending by time, for
/// `get_messages`. Asks one row past the page so `complete` and
/// `next_cursor` know whether older history is still in the archive.
pub(crate) fn page_messages(
    archive: &crate::archive::Archive,
    chat: &str,
    before: Option<(i64, &str)>,
    limit: usize,
) -> Result<dto::MessagesResponse, ReadError> {
    let known = archive
        .chat(chat)?
        .ok_or_else(|| ReadUnavailable::UnknownChat(chat.to_owned()))?;
    if known.locked {
        return Err(ReadUnavailable::Locked.into());
    }
    let mut rows = archive.messages(chat, before, limit + 1)?;
    let next_cursor = (rows.len() > limit).then(|| {
        let oldest = rows.remove(0);
        dto::Cursor {
            timestamp: oldest.timestamp,
            id: oldest.id,
        }
    });
    let complete = next_cursor.is_none();
    Ok(dto::MessagesResponse {
        chat: chat.to_owned(),
        messages: rows.into_iter().map(dto::MessageDto::from).collect(),
        next_cursor,
        complete,
    })
}

/// Archive search for `search_messages`, over one chat or every unlocked
/// one, with locked chats' hits taken back out.
pub(crate) fn search_messages(
    archive: &crate::archive::Archive,
    query: &str,
    chat: Option<&str>,
    from: Option<i64>,
    until: Option<i64>,
    limit: usize,
) -> Result<dto::SearchResponse, ReadError> {
    let mut rows: Vec<_> = if let Some(chat) = chat {
        let known = archive
            .chat(chat)?
            .ok_or_else(|| ReadUnavailable::UnknownChat(chat.to_owned()))?;
        if known.locked {
            return Err(ReadUnavailable::Locked.into());
        }
        archive.search_chat_messages(chat, query, from, until, limit)?
    } else {
        archive.search_messages(query, limit)?
    };
    // Locked chats stay unreadable even to a global search. Their chat rows
    // are read here, never their content.
    let locked: Vec<String> = archive
        .chats()?
        .into_iter()
        .filter(|chat| chat.locked)
        .map(|chat| chat.id)
        .collect();
    let before = rows.len();
    if !locked.is_empty() {
        rows.retain(|row| !locked.contains(&row.chat));
    }
    let truncated = rows.len() != before || before == limit;
    Ok(dto::SearchResponse {
        messages: rows.into_iter().map(dto::MessageDto::from).collect(),
        truncated,
    })
}

impl AccountEntry {
    fn send(&self, command: Command) -> Result<(), String> {
        self.commands
            .send(command)
            .map_err(|_| "This account's worker is not running".to_owned())
    }

    /// Opens a fresh read-only archive reader. Never migrates, never creates.
    fn open_reader(&self) -> Result<crate::archive::Archive, String> {
        crate::archive::Archive::export_reader(&self.archive_db).map_err(|error| {
            log::warn!("MCP archive reader did not open: {error:#}");
            "The message archive is not available".to_owned()
        })
    }

    /// Runs one read against the cached reader, reopening once when SQLite
    /// fails. Call only from a blocking task: the lock and the query block.
    fn with_archive<T>(
        &self,
        read: impl Fn(&crate::archive::Archive) -> Result<T, ReadError>,
    ) -> Result<T, String> {
        let mut slot = self.reader.lock().unwrap_or_else(|p| p.into_inner());
        if slot.is_none() {
            *slot = Some(self.open_reader()?);
        }
        match read(slot.as_ref().expect("reader just ensured")) {
            Ok(value) => Ok(value),
            Err(ReadError::Unavailable(reason)) => Err(reason),
            Err(ReadError::Sql(error)) => {
                log::warn!("MCP archive query failed, reopening: {error}");
                *slot = Some(self.open_reader()?);
                read(slot.as_ref().expect("reader just reopened")).map_err(|error| match error {
                    ReadError::Unavailable(reason) => reason,
                    ReadError::Sql(error) => {
                        log::warn!("MCP archive query failed again: {error}");
                        "The message archive could not be read".to_owned()
                    }
                })
            }
        }
    }
}

/// The accounts the MCP server may serve, shared between the app and the
/// server thread.
#[derive(Default)]
pub struct Hub {
    accounts: Mutex<HashMap<AccountId, Arc<AccountEntry>>>,
}

impl Hub {
    pub fn new() -> Self {
        Self::default()
    }

    /// Offers one linked account to the MCP server. The command sender is the
    /// same channel the interface uses, so MCP sends pass every worker check.
    pub fn register(
        &self,
        id: AccountId,
        dirs: AccountDirs,
        sender: tokio::sync::mpsc::UnboundedSender<Command>,
    ) {
        self.accounts
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(
                id,
                Arc::new(AccountEntry {
                    commands: sender,
                    archive_db: dirs.archive_db(),
                    reader: Mutex::new(None),
                }),
            );
    }

    /// Forgets an account whose backend has stopped, dropping its reader.
    pub fn unregister(&self, id: &AccountId) {
        self.accounts
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(id);
    }

    /// Every offered account, ordered by id for a stable `list_accounts`.
    pub fn ids(&self) -> Vec<AccountId> {
        let accounts = self.accounts.lock().unwrap_or_else(|p| p.into_inner());
        let mut ids: Vec<AccountId> = accounts.keys().cloned().collect();
        ids.sort_by(|a, b| a.0.cmp(&b.0));
        ids
    }

    /// Resolves the `account_id` argument of a tool, or the first account
    /// when the tool call names none.
    fn entry(&self, account: Option<&str>) -> Result<(AccountId, Arc<AccountEntry>), String> {
        let accounts = self.accounts.lock().unwrap_or_else(|p| p.into_inner());
        match account {
            Some(name) => {
                let id = AccountId::from(name);
                let entry = accounts
                    .get(&id)
                    .ok_or_else(|| format!("Unknown account id {name}; call list_accounts"))?;
                Ok((id, entry.clone()))
            }
            None => {
                let mut ids: Vec<&AccountId> = accounts.keys().collect();
                ids.sort_by(|a, b| a.0.cmp(&b.0));
                let id = (*ids
                    .first()
                    .ok_or_else(|| "No WhatsApp account is linked in ZapFast".to_owned())?)
                .clone();
                let entry = accounts[&id].clone();
                Ok((id, entry))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHAT: &str = "15550002222@s.whatsapp.net";

    /// An in-memory archive with one chat and five text messages, oldest
    /// first, filed the way the worker files them.
    fn archive() -> crate::archive::Archive {
        let archive = crate::archive::Archive::in_memory().expect("archive");
        archive.ensure_chat(CHAT, "Ada").expect("chat");
        for n in 0..5 {
            archive.execute_for_tests(&format!(
                "INSERT INTO messages (chat, id, sender, from_me, timestamp, content) \
                     VALUES ('{CHAT}', 'm{n}', '{CHAT}', 0, {time}, \
                     '{{\"kind\":\"text\",\"text\":\"message {n}\"}}')",
                time = 1_700_000_000 + n * 10
            ));
        }
        archive
    }

    #[test]
    fn pages_ascending_with_a_cursor_for_older_history() {
        let archive = archive();
        let page = page_messages(&archive, CHAT, None, 3).expect("page");
        assert_eq!(page.messages.len(), 3);
        assert!(!page.complete);
        let cursor = page.next_cursor.clone().expect("older history");
        assert_eq!(cursor.id, "m1");
        assert_eq!(
            page.messages[0].content,
            crate::model::Content::text("message 2")
        );
        // The next page starts at the boundary and holds what is older.
        let older = page_messages(&archive, CHAT, Some((cursor.timestamp, &cursor.id)), 3)
            .expect("older page");
        assert_eq!(
            older
                .messages
                .iter()
                .map(|m| m.id.clone())
                .collect::<Vec<_>>(),
            ["m0"]
        );
        assert!(older.complete);
        assert!(older.next_cursor.is_none());
    }

    #[test]
    fn a_page_at_the_archive_end_is_complete() {
        let archive = archive();
        let page = page_messages(&archive, CHAT, None, 10).expect("page");
        assert_eq!(page.messages.len(), 5);
        assert!(page.complete);
        assert!(page.next_cursor.is_none());
    }

    #[test]
    fn an_unknown_chat_is_named_in_plain_words() {
        let archive = archive();
        let error =
            page_messages(&archive, "1999@s.whatsapp.net", None, 10).expect_err("unknown chat");
        match error {
            ReadError::Unavailable(reason) => {
                assert!(reason.contains("1999@s.whatsapp.net"), "{reason}");
            }
            ReadError::Sql(_) => panic!("a missing chat is not a SQLite failure"),
        }
    }

    #[test]
    fn a_locked_chat_is_never_read_or_searched() {
        let archive = archive();
        archive.set_locked(CHAT, true).expect("lock");
        let read = page_messages(&archive, CHAT, None, 10).expect_err("locked");
        let search = search_messages(&archive, "message", Some(CHAT), None, None, 10)
            .expect_err("locked search");
        for error in [read, search] {
            match error {
                ReadError::Unavailable(reason) => {
                    assert!(reason.contains("locked"), "{reason}");
                }
                ReadError::Sql(_) => panic!("a locked chat is not a SQLite failure"),
            }
        }
    }

    #[test]
    fn search_finds_text_and_reports_truncation() {
        let archive = archive();
        let hits =
            search_messages(&archive, "message 4", None, None, None, 10).expect("global search");
        assert_eq!(hits.messages.len(), 1);
        assert_eq!(hits.messages[0].id, "m4");
        assert!(!hits.truncated);
        let capped = search_messages(&archive, "message", None, None, None, 2).expect("capped");
        assert_eq!(capped.messages.len(), 2);
        assert!(capped.truncated);
        // A per-chat search with an empty query lists the chat in range.
        let ranged = search_messages(&archive, "", Some(CHAT), None, None, 10).expect("range");
        assert_eq!(ranged.messages.len(), 5);
    }

    #[test]
    fn locked_chats_hits_are_dropped_from_a_global_search() {
        let archive = crate::archive::Archive::in_memory().expect("archive");
        archive.ensure_chat(CHAT, "Ada").expect("chat");
        let locked_chat = "15550003333@s.whatsapp.net";
        archive.ensure_chat(locked_chat, "Secret").expect("chat");
        archive.set_locked(locked_chat, true).expect("lock");
        for (chat, id, text) in [
            (CHAT, "a1", "meeting at noon"),
            (locked_chat, "b1", "meeting at noon"),
        ] {
            archive.execute_for_tests(&format!(
                "INSERT INTO messages (chat, id, sender, from_me, timestamp, content) \
                     VALUES ('{chat}', '{id}', '{chat}', 0, 1_700_000_000, \
                     '{{\"kind\":\"text\",\"text\":\"{text}\"}}')"
            ));
        }
        let hits = search_messages(&archive, "meeting", None, None, None, 10).expect("hits");
        assert_eq!(hits.messages.len(), 1);
        assert_eq!(hits.messages[0].chat, CHAT);
        assert!(hits.truncated, "dropped hits are reported");
    }

    #[test]
    fn the_hub_resolves_explicit_and_default_accounts() {
        let hub = Hub::new();
        assert!(hub.entry(None).is_err(), "no account registered yet");
        let root = tempfile::tempdir().expect("temp dir");
        let dirs = crate::paths::AppDirs::under(root.path()).as_account();
        let (commands, _inbox) = tokio::sync::mpsc::unbounded_channel();
        hub.register(AccountId::from("2"), dirs.clone(), commands.clone());
        let (commands, _inbox) = tokio::sync::mpsc::unbounded_channel();
        hub.register(AccountId::from("1"), dirs.clone(), commands);
        // The default is the first account by id, not the registration order.
        let (id, _) = hub.entry(None).expect("default account");
        assert_eq!(id, AccountId::from("1"));
        let (id, _) = hub.entry(Some("2")).expect("named account");
        assert_eq!(id, AccountId::from("2"));
        assert!(hub.entry(Some("3")).is_err());
        // Dropping an account takes it back out.
        hub.unregister(&AccountId::from("2"));
        assert_eq!(hub.ids(), [AccountId::from("1")]);
    }
}
