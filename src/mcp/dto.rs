//! The JSON shapes the MCP tools answer with.
//!
//! Timestamps are Unix seconds, as the model keeps them. Message content
//! reuses [`crate::model::Content`]'s own serde form (a `kind` tag, lowercase
//! names), so the wire stays in step with what the archive files; the JPEG
//! thumbnails, which ride on the message and not the content, never cross.
//! Media carries its local path only when the file is already downloaded: a
//! tool call never starts a download.

use serde::Serialize;

use crate::model::{Chat, ChatKind, Delivery, Message};

/// One linked account as `list_accounts` names it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AccountDto {
    pub id: String,
    /// The linked phone number as a JID, once the connection has said it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jid: Option<String>,
    /// The profile name, once known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// One word: starting, unlinked, connecting, connected, disconnected,
    /// logged_out, failed, or stopped (the worker did not answer).
    pub status: &'static str,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AccountsResponse {
    pub accounts: Vec<AccountDto>,
}

/// A chat as `list_chats` and `search_messages` name it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ChatDto {
    pub id: String,
    pub name: String,
    pub kind: ChatKind,
    /// Latest-message Unix seconds, the chat list's own order.
    pub last_activity: i64,
    pub unread: u32,
    pub archived: bool,
    pub pinned: bool,
    pub favorite: bool,
    pub locked: bool,
    /// An announcement group we cannot post in, or a chat we left.
    pub read_only: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub muted_until: Option<i64>,
    /// Disappearing-message timer in seconds, when the chat has said one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ephemeral_expiration: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_message: Option<LastMessageDto>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LastMessageDto {
    pub from_me: bool,
    pub sender: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sender_name: Option<String>,
    pub summary: String,
    pub status: Delivery,
}

impl ChatDto {
    /// Takes the fields a tool caller may see, leaving settings, group
    /// metadata, and labels to the app.
    pub fn from_chat(chat: &Chat) -> Self {
        Self {
            id: chat.id.clone(),
            name: chat.name.clone(),
            kind: chat.kind,
            last_activity: chat.last_activity,
            unread: chat.unread,
            archived: chat.archived,
            pinned: chat.pinned,
            favorite: chat.favorite,
            locked: chat.locked,
            read_only: chat.read_only,
            muted_until: chat.muted_until,
            ephemeral_expiration: chat.ephemeral_expiration,
            last_message: chat.last.as_ref().map(|last| LastMessageDto {
                from_me: last.from_me,
                sender: last.sender.clone(),
                sender_name: last.sender_name.clone(),
                summary: last.summary.clone(),
                status: last.status,
            }),
        }
    }
}

/// Filters and cuts `chats` for `list_chats`: archived and locked chats stay
/// out unless they are asked for, the name filter matches as plain text, and
/// the caller's order (newest first, straight from the archive) is kept.
pub fn collect_chats(
    chats: Vec<Chat>,
    filter: Option<&str>,
    include_archived: bool,
    include_locked: bool,
    limit: usize,
) -> Vec<ChatDto> {
    let needle = filter.map(|f| f.to_ascii_lowercase());
    chats
        .into_iter()
        .filter(|chat| {
            (include_archived || !chat.archived)
                && (include_locked || !chat.locked)
                && needle
                    .as_deref()
                    .is_none_or(|needle| chat.name.to_ascii_lowercase().contains(needle))
        })
        .take(limit)
        .map(|chat| ChatDto::from_chat(&chat))
        .collect()
}

/// A message as `get_messages` and `search_messages` name it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MessageDto {
    pub id: String,
    pub chat: String,
    /// Sender JID, ours for outgoing messages.
    pub sender: String,
    /// Group sender's push name, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sender_name: Option<String>,
    pub from_me: bool,
    pub timestamp: i64,
    pub status: Delivery,
    /// Tagged content; a `media` object names its local file under `path`
    /// only when that file is already downloaded.
    pub content: crate::model::Content,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quoted: Option<crate::model::Quoted>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub reactions: Vec<crate::model::Reaction>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub edited: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub forwarded: bool,
}

impl From<Message> for MessageDto {
    /// Takes the row apart, so nothing large (its JPEG thumbnail) is cloned.
    fn from(row: Message) -> Self {
        Self {
            id: row.id,
            chat: row.chat,
            sender: row.sender,
            sender_name: row.sender_name,
            from_me: row.from_me,
            timestamp: row.timestamp,
            status: row.status,
            content: row.content,
            quoted: row.quoted,
            reactions: row.reactions,
            edited: row.edited,
            forwarded: row.forwarded,
        }
    }
}

/// Where the next, older page starts: the page's oldest message.
#[derive(Clone, Debug, PartialEq, Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct Cursor {
    /// Unix seconds of the boundary message.
    pub timestamp: i64,
    /// The boundary message's id, unique within its chat.
    pub id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ChatsResponse {
    /// Newest activity first.
    pub chats: Vec<ChatDto>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MessagesResponse {
    pub chat: String,
    /// Ascending by time. Up to `limit` rows, and when `complete` is false
    /// one more page exists behind `next_cursor`.
    pub messages: Vec<MessageDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<Cursor>,
    /// Whether every locally archived message of this chat is in this page
    /// (or the pages before it). ZapFast's archive may lag the phone.
    pub complete: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SearchResponse {
    /// Newest first, the archive's own search order.
    pub messages: Vec<MessageDto>,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SendResponse {
    pub chat: String,
    /// The id WhatsApp's server sees, as returned by ZapFast's send path.
    pub message_id: String,
    pub status: Delivery,
    /// When the acknowledgement has not arrived within the wait, the message
    /// may still deliver later.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ChatId, LastMessage, Reaction};

    fn chat(id: ChatId, name: &str, locked: bool, archived: bool) -> Chat {
        let kind = ChatKind::from_id(&id);
        Chat {
            id,
            name: name.to_owned(),
            group_subject_known: true,
            kind,
            last_activity: 100,
            unread: 2,
            marked_unread: false,
            archived,
            pinned: false,
            pinned_at: 0,
            muted_until: None,
            last: None,
            favorite: false,
            favorite_position: 0,
            participants: Vec::new(),
            group_description: None,
            read_only: false,
            left: false,
            info_locked: None,
            admin: false,
            group_settings: None,
            locked,
            ephemeral_expiration: None,
            labels: Vec::new(),
            notification_sound: None,
            unread_mention: None,
            voice_speed: None,
        }
    }

    #[test]
    fn chat_json_holds_the_fields_a_tool_caller_needs() {
        let mut chat = chat("15550001111@s.whatsapp.net".into(), "Ada", false, false);
        chat.last = Some(LastMessage {
            from_me: false,
            sender: "15550001111@s.whatsapp.net".into(),
            sender_name: Some("Ada".into()),
            summary: "hello".into(),
            full: "hello".into(),
            labelled: false,
            status: Delivery::None,
        });
        let json = serde_json::to_value(ChatDto::from_chat(&chat)).unwrap();
        assert_eq!(json["id"], "15550001111@s.whatsapp.net");
        assert_eq!(json["name"], "Ada");
        assert_eq!(json["kind"], "direct");
        assert_eq!(json["unread"], 2);
        assert_eq!(json["last_message"]["summary"], "hello");
        assert_eq!(json["last_message"]["status"], "none");
        // Fields the app keeps for itself do not cross.
        assert!(json.get("participants").is_none());
        assert!(json.get("labels").is_none());
        assert!(json.get("group_settings").is_none());
    }

    #[test]
    fn collect_chats_filters_archived_locked_and_names() {
        let chats = vec![
            chat("100@s.whatsapp.net".into(), "Ada", false, false),
            chat("200@s.whatsapp.net".into(), "Bob", true, false),
            chat("300@s.whatsapp.net".into(), "Ann archived", false, true),
        ];
        let all = collect_chats(chats.clone(), None, false, false, 100);
        assert_eq!(
            all.iter().map(|c| c.id.clone()).collect::<Vec<_>>(),
            ["100@s.whatsapp.net"]
        );
        let with_locked = collect_chats(chats.clone(), None, false, true, 100);
        assert_eq!(with_locked.len(), 2);
        let with_archived = collect_chats(chats.clone(), None, true, false, 100);
        assert_eq!(with_archived.len(), 2);
        let filtered = collect_chats(chats, Some("ad"), false, true, 100);
        assert_eq!(
            filtered.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
            ["Ada"]
        );
    }

    #[test]
    fn collect_chats_cuts_at_the_limit() {
        let chats: Vec<Chat> = (0..5)
            .map(|n| chat(format!("{n}00@s.whatsapp.net"), "Chat", false, false))
            .collect();
        assert_eq!(collect_chats(chats, None, false, false, 2).len(), 2);
    }

    #[test]
    fn message_json_holds_content_without_the_thumbnail() {
        let row = Message {
            id: "3EB0".into(),
            chat: "15550001111@s.whatsapp.net".into(),
            sender: "15550001111@s.whatsapp.net".into(),
            sender_name: Some("Ada".into()),
            from_me: false,
            timestamp: 1_700_000_000,
            history_order: None,
            content: crate::model::Content::Text {
                text: "hello".into(),
                preview: None,
            },
            status: Delivery::None,
            delivered_at: None,
            read_at: None,
            quoted: None,
            reactions: vec![Reaction {
                sender: "15550001111@s.whatsapp.net".into(),
                from_me: false,
                emoji: "👍".into(),
            }],
            edited: false,
            mentions: Vec::new(),
            forwarded: false,
            thumbnail: Some(vec![0xff; 1024]),
        };
        let json = serde_json::to_value(MessageDto::from(row)).unwrap();
        assert_eq!(json["id"], "3EB0");
        assert_eq!(json["content"]["kind"], "text");
        assert_eq!(json["content"]["text"], "hello");
        assert_eq!(json["reactions"][0]["emoji"], "👍");
        assert!(json.get("thumbnail").is_none());
        assert!(json.get("mentions").is_none());
    }

    #[test]
    fn message_json_names_a_downloaded_media_file() {
        let row = Message {
            id: "3EB1".into(),
            chat: "15550001111@s.whatsapp.net".into(),
            sender: "15550001111@s.whatsapp.net".into(),
            sender_name: None,
            from_me: false,
            timestamp: 1_700_000_001,
            history_order: None,
            content: crate::model::Content::Image {
                caption: None,
                media: crate::model::Media {
                    mime: "image/jpeg".into(),
                    size: 10_000,
                    width: Some(800),
                    height: Some(600),
                    album: None,
                    path: Some("/tmp/media/3EB1.jpg".into()),
                    state: crate::model::MediaState::default(),
                },
                motion: None,
            },
            status: Delivery::None,
            delivered_at: None,
            read_at: None,
            quoted: None,
            reactions: Vec::new(),
            edited: false,
            mentions: Vec::new(),
            forwarded: false,
            thumbnail: None,
        };
        let json = serde_json::to_value(MessageDto::from(row)).unwrap();
        assert_eq!(json["content"]["kind"], "image");
        assert_eq!(json["content"]["media"]["mime"], "image/jpeg");
        assert_eq!(json["content"]["media"]["path"], "/tmp/media/3EB1.jpg");
    }
}
