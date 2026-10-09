use anyhow::{Context as _, Result, bail};
use atomicwrites::{AtomicFile, OverwriteBehavior};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use specta::Type;
use std::{
    io::Write as _,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct ChatMessage {
    pub role: String,
    pub text: String,
    pub error: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct Chat {
    pub id: Uuid,
    pub title: String,
    pub updated_at: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    #[specta(skip)]
    pub(crate) context: Vec<Value>,
}

#[derive(Default, Deserialize, Serialize)]
struct Archive {
    active: Option<Uuid>,
    chats: Vec<Chat>,
}

pub(crate) struct History {
    path: PathBuf,
    archive: Archive,
}

impl History {
    pub(crate) fn open(root: &Path, scope: &str) -> Result<Self> {
        let path = root.join(format!("{}.json", blake3::hash(scope.as_bytes())));
        let archive = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .context("could not read saved Agent chats; original file preserved")?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Archive::default(),
            Err(error) => return Err(error.into()),
        };
        Ok(Self { path, archive })
    }

    pub(crate) fn chats(&self) -> Vec<Chat> {
        let mut chats = self.archive.chats.clone();
        chats.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        for chat in &mut chats {
            chat.context.clear();
        }
        chats
    }

    pub(crate) fn active(&self) -> Option<&Chat> {
        self.archive
            .chats
            .iter()
            .find(|chat| Some(chat.id) == self.archive.active)
    }

    pub(crate) fn context(&self) -> Vec<Value> {
        self.active()
            .map(|chat| chat.context.clone())
            .unwrap_or_default()
    }

    pub(crate) fn start(&mut self) -> Result<Chat> {
        let chat = Chat {
            id: Uuid::new_v4(),
            title: "New chat".into(),
            updated_at: now(),
            messages: Vec::new(),
            context: Vec::new(),
        };
        self.archive.active = Some(chat.id);
        self.archive.chats.push(chat.clone());
        self.save()?;
        Ok(chat)
    }

    pub(crate) fn select(&mut self, id: Uuid) -> Result<Chat> {
        let chat = self
            .archive
            .chats
            .iter()
            .find(|chat| chat.id == id)
            .context("saved chat does not exist in this project")?
            .clone();
        self.archive.active = Some(id);
        self.save()?;
        Ok(chat)
    }

    pub(crate) fn begin(&mut self, prompt: &str) -> Result<()> {
        if self.active().is_none() {
            self.start()?;
        }
        let chat = self.active_mut()?;
        if chat.messages.is_empty() {
            chat.title = prompt.chars().take(70).collect();
        }
        chat.updated_at = now();
        chat.messages.push(ChatMessage {
            role: "user".into(),
            text: prompt.into(),
            error: false,
        });
        chat.messages.push(ChatMessage {
            role: "assistant".into(),
            text: "Request interrupted before completion.".into(),
            error: true,
        });
        self.save()
    }

    pub(crate) fn finish(&mut self, text: &str, error: bool, context: Vec<Value>) -> Result<()> {
        let chat = self.active_mut()?;
        let response = chat
            .messages
            .last_mut()
            .context("no Agent request to finish")?;
        if response.role != "assistant" {
            bail!("last chat entry is not an Agent response");
        }
        response.text = text.into();
        response.error = error;
        chat.context = context;
        chat.updated_at = now();
        self.save()
    }

    fn active_mut(&mut self) -> Result<&mut Chat> {
        self.archive
            .chats
            .iter_mut()
            .find(|chat| Some(chat.id) == self.archive.active)
            .context("no chat selected")
    }

    fn save(&self) -> Result<()> {
        let parent = self.path.parent().context("chat archive has no parent")?;
        std::fs::create_dir_all(parent)?;
        let bytes = serde_json::to_vec_pretty(&self.archive)?;
        AtomicFile::new(&self.path, OverwriteBehavior::AllowOverwrite)
            .write(|file| {
                file.write_all(&bytes)?;
                file.sync_all()
            })
            .map_err(|error| anyhow::anyhow!("could not save Agent chat: {error}"))
    }
}

fn now() -> String {
    format!(
        "{:020}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn restores_selected_chat_and_model_context_after_restart() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = History::open(directory.path(), "account/project").unwrap();
        let id = store.start().unwrap().id;
        store.begin("Remember the name Elser").unwrap();
        store
            .finish(
                "I will use Elser",
                false,
                vec![json!({"role":"user","content":"Remember the name Elser"})],
            )
            .unwrap();
        drop(store);
        let restored = History::open(directory.path(), "account/project").unwrap();
        assert_eq!(restored.active().unwrap().id, id);
        assert_eq!(restored.active().unwrap().messages.len(), 2);
        assert_eq!(restored.context()[0]["content"], "Remember the name Elser");
    }

    #[test]
    fn switching_chats_keeps_contexts_separate() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = History::open(directory.path(), "a/p").unwrap();
        let first = store.start().unwrap().id;
        store.begin("First").unwrap();
        store
            .finish(
                "Reply",
                false,
                vec![json!({"role":"user","content":"First"})],
            )
            .unwrap();
        store.start().unwrap();
        assert!(store.context().is_empty());
        store.select(first).unwrap();
        assert_eq!(store.context().len(), 1);
        assert_eq!(store.active().unwrap().messages[0].text, "First");
        assert!(
            History::open(directory.path(), "other/project")
                .unwrap()
                .chats()
                .is_empty()
        );
    }

    #[test]
    fn interrupted_turn_is_visible_and_corrupt_history_is_not_overwritten() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = History::open(directory.path(), "a/p").unwrap();
        store.begin("A request").unwrap();
        let restored = History::open(directory.path(), "a/p").unwrap();
        assert!(restored.active().unwrap().messages[1].error);
        std::fs::write(&store.path, "invalid json").unwrap();
        assert!(History::open(directory.path(), "a/p").is_err());
        assert_eq!(
            std::fs::read_to_string(&store.path).unwrap(),
            "invalid json"
        );
    }
}
