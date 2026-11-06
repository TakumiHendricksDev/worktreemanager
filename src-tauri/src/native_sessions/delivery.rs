//! A lost receipt is not permission to replay an implementation instruction. Persist the
//! uncertain state before writing to a provider, and only settle it on a correlated reply.

use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStatus {
    Pending,
    Unknown,
    Delivered,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Owner {
    pub project: String,
    pub worktree: String,
    pub provider: String,
    pub conversation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Delivery {
    pub id: String,
    pub owner: Owner,
    pub label: String,
    pub status: DeliveryStatus,
    pub error: Option<String>,
    pub prototype: Option<String>,
    pub review: Option<String>,
    // This payload never goes to the webview through Snapshot; only the intended provider.
    pub text: String,
}

#[derive(Debug)]
pub struct Store {
    root: PathBuf,
    mutation: parking_lot::Mutex<()>,
}

impl Store {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            mutation: parking_lot::Mutex::new(()),
        }
    }

    pub fn list(&self, owner: &Owner) -> Vec<Delivery> {
        self.checked_list(owner).unwrap_or_default()
    }

    /// A corrupt journal cannot be treated as proof an instruction was never dispatched.
    pub fn checked_list(&self, owner: &Owner) -> Result<Vec<Delivery>, String> {
        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.to_string()),
        };
        let mut values = Vec::new();
        for entry in entries {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.extension().and_then(|p| p.to_str()) != Some("json") {
                continue;
            }
            let id = path
                .file_stem()
                .and_then(|p| p.to_str())
                .ok_or("Invalid receipt filename")?;
            let value = self.read(id).map_err(|e| {
                format!("Delivery journal cannot be verified: {e}. Nothing will be sent.")
            })?;
            if value.owner == *owner {
                values.push(value);
            }
        }
        Ok(values)
    }

    pub fn prepare(
        &self,
        owner: Owner,
        label: &str,
        text: &str,
        review: Option<(&str, &str)>,
    ) -> Result<Delivery, String> {
        let _guard = self.mutation.lock();
        if owner.conversation.is_empty() || text.is_empty() || text.len() > 256_000 {
            return Err("A registered conversation and a bounded message are required".into());
        }
        if let Some((prototype, revision)) = review
            && let Some(existing) = self.checked_list(&owner)?.into_iter().find(|d| {
                d.prototype.as_deref() == Some(prototype)
                    && d.review.as_deref() == Some(revision)
                    && d.status != DeliveryStatus::Rejected
            })
        {
            return Ok(existing);
        }
        if self
            .checked_list(&owner)?
            .iter()
            .any(|d| matches!(d.status, DeliveryStatus::Pending | DeliveryStatus::Unknown))
        {
            return Err(
                "Resolve the pending or uncertain delivery in this conversation first".into(),
            );
        }
        let delivery = Delivery {
            id: uuid::Uuid::new_v4().to_string(),
            owner,
            label: label.to_owned(),
            text: text.to_owned(),
            status: DeliveryStatus::Pending,
            error: None,
            prototype: review.map(|r| r.0.to_owned()),
            review: review.map(|r| r.1.to_owned()),
        };
        self.save(&delivery)?;
        Ok(delivery)
    }

    pub fn transition(
        &self,
        id: &str,
        owner: &Owner,
        from: DeliveryStatus,
        to: DeliveryStatus,
        error: Option<String>,
    ) -> Result<Delivery, String> {
        let _guard = self.mutation.lock();
        let mut delivery = self.read(id)?;
        if delivery.owner != *owner {
            return Err("Delivery belongs to another conversation".into());
        }
        if delivery.status != from {
            return Err("Delivery has already changed; it will not be replayed".into());
        }
        delivery.status = to;
        delivery.error = error;
        self.save(&delivery)?;
        Ok(delivery)
    }

    pub fn read(&self, id: &str) -> Result<Delivery, String> {
        let id = uuid::Uuid::parse_str(id).map_err(|_| "Invalid receipt")?;
        let path = self.root.join(format!("{id}.json"));
        if std::fs::symlink_metadata(&path)
            .is_ok_and(|m| !m.is_file() || m.file_type().is_symlink())
        {
            return Err("Linked receipt records are excluded".into());
        }
        let mut text = String::new();
        std::fs::File::open(path)
            .map_err(|e| e.to_string())?
            .take(512_000)
            .read_to_string(&mut text)
            .map_err(|e| e.to_string())?;
        serde_json::from_str(&text).map_err(|e| e.to_string())
    }

    /// Public history can positively confirm a lost reply. Absence is never proof of rejection:
    /// Codex may return only part of a thread, and the host must not authorize a replay from it.
    pub fn reconcile_codex(
        &self,
        owner: &Owner,
        thread: &serde_json::Value,
    ) -> Result<bool, String> {
        let _guard = self.mutation.lock();
        if owner.provider != "codex" || thread["id"].as_str() != Some(&owner.conversation) {
            return Err("Provider history belongs to another conversation".into());
        }
        let mut changed = false;
        for mut delivery in self.checked_list(owner)? {
            if delivery.status != DeliveryStatus::Unknown {
                continue;
            }
            let found = thread["turns"].as_array().is_some_and(|turns| {
                turns.iter().any(|turn| {
                    turn["items"].as_array().is_some_and(|items| {
                        items.iter().any(|item| {
                            item["type"] == "userMessage"
                                && item["clientId"].as_str() == Some(&delivery.id)
                                && item["content"].as_array().is_some_and(|content| {
                                    content.len() == 1
                                        && content[0]["type"] == "text"
                                        && content[0]["text"].as_str() == Some(&delivery.text)
                                })
                        })
                    })
                })
            });
            if found {
                delivery.status = DeliveryStatus::Delivered;
                delivery.error = None;
                self.save(&delivery)?;
                changed = true;
            }
        }
        Ok(changed)
    }

    fn save(&self, delivery: &Delivery) -> Result<(), String> {
        crate::prototypes::private_dir(&self.root).map_err(|e| e.message)?;
        let target = self.root.join(format!("{}.json", delivery.id));
        let temporary = self.root.join(format!("{}.tmp", uuid::Uuid::new_v4()));
        write_durable(
            &temporary,
            &serde_json::to_vec(delivery).map_err(|e| e.to_string())?,
        )?;
        std::fs::rename(&temporary, target).map_err(|e| e.to_string())?;
        std::fs::File::open(&self.root)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())
    }
}

fn write_durable(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|e| e.to_string())
}
