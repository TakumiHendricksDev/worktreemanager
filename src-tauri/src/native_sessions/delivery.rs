//! A lost receipt is not permission to replay an implementation instruction. Persist the
//! uncertain state before writing to a provider, and only settle it on a correlated reply.
//!
//! The journal is read once and then served from memory, written through on every change. Every
//! activity update announces its session's receipts from the provider's reader thread, and
//! re-reading every receipt ever written there is what made a busy turn slow. Writers still
//! serialize on their own lock across the disk write; readers only wait for the brief swap after.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

/// How long a delivered or rejected receipt is kept. Long enough to answer "did that design go
/// out?" about last month's work; an unresolved receipt is never pruned by age, because it is the
/// only record that an instruction may be sitting in a conversation.
pub const SETTLED_RETENTION: Duration = Duration::from_secs(30 * 24 * 60 * 60);

const NOT_DELIVERED: &str = "You checked the conversation and marked this instruction as not delivered. wtm did not resend it.";
const WITHDRAWN: &str = "You withdrew this instruction before the native CLI collected it.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStatus {
    Pending,
    Unknown,
    Delivered,
    Rejected,
}

/// Why a receipt the provider never answered was settled by the user rather than by evidence.
///
/// Recorded beside the status because "rejected by the provider" and "you said it never arrived"
/// call for different next steps, and only the second can be overturned by a late reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resolution {
    /// `unknown` → `rejected`: the user looked and did not find it.
    NotDelivered,
    /// `pending` → `rejected`: the user took it back before anything collected it.
    Withdrawn,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<Resolution>,
    // This payload never goes to the webview through Snapshot; only the intended provider.
    pub text: String,
}

impl Delivery {
    #[must_use]
    pub fn unresolved(&self) -> bool {
        matches!(
            self.status,
            DeliveryStatus::Pending | DeliveryStatus::Unknown
        )
    }
}

type Receipts = BTreeMap<String, Delivery>;

#[derive(Debug)]
enum Journal {
    Loaded(Receipts),
    /// A corrupt journal cannot be treated as proof an instruction was never dispatched, so
    /// nothing is sent while this is the state.
    Unreadable(String),
}

#[derive(Debug)]
pub struct Store {
    root: PathBuf,
    /// Held across a disk write, so two writers cannot both pass a check and both persist.
    mutation: Mutex<()>,
    journal: Mutex<Journal>,
}

impl Store {
    pub fn new(root: PathBuf) -> Self {
        let journal = load(&root);
        Self {
            root,
            mutation: Mutex::new(()),
            journal: Mutex::new(journal),
        }
    }

    /// The owner's receipts as last written. Empty when the journal could not be read.
    pub fn list(&self, owner: &Owner) -> Vec<Delivery> {
        self.cached(|all| owned(all, owner).cloned().collect())
            .unwrap_or_default()
    }

    /// A corrupt journal cannot be treated as proof an instruction was never dispatched.
    pub fn checked_list(&self, owner: &Owner) -> Result<Vec<Delivery>, String> {
        let _guard = self.mutation.lock();
        self.writable()?;
        self.cached(|all| owned(all, owner).cloned().collect())
    }

    /// Everything about `owner`'s conversation, and what is still unresolved in the conversations
    /// this pane had before it. Memory only, because it runs on every activity update.
    ///
    /// An earlier conversation's settled receipts are left out: they ask nothing of the user, and
    /// the conversation that owns them lists them when it is resumed.
    pub fn receipts(&self, owner: &Owner, earlier: &[String]) -> Result<Vec<Delivery>, String> {
        self.cached(|all| {
            all.values()
                .filter(|d| {
                    d.owner.project == owner.project
                        && d.owner.worktree == owner.worktree
                        && d.owner.provider == owner.provider
                        && (d.owner.conversation == owner.conversation
                            || d.unresolved() && earlier.contains(&d.owner.conversation))
                })
                .cloned()
                .collect()
        })
    }

    /// The live receipt for one reviewed design in this conversation, if one was ever prepared.
    pub fn existing(
        &self,
        owner: &Owner,
        prototype: &str,
        review: &str,
    ) -> Result<Option<Delivery>, String> {
        self.cached(|all| {
            owned(all, owner)
                .find(|d| {
                    d.prototype.as_deref() == Some(prototype)
                        && d.review.as_deref() == Some(review)
                        && d.status != DeliveryStatus::Rejected
                })
                .cloned()
        })
    }

    /// The receipt that says what happened to a reviewed design, whichever conversation sent it.
    /// A live one wins over a rejected attempt, because that attempt is history.
    pub fn for_review(&self, prototype: &str, review: &str) -> Option<Delivery> {
        self.cached(|all| {
            let mut found: Option<&Delivery> = None;
            for d in all.values().filter(|d| {
                d.prototype.as_deref() == Some(prototype) && d.review.as_deref() == Some(review)
            }) {
                if found.is_none_or(|f| f.status == DeliveryStatus::Rejected) {
                    found = Some(d);
                }
            }
            found.cloned()
        })
        .ok()
        .flatten()
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
        self.writable()?;
        if let Some((prototype, revision)) = review
            && let Some(existing) = self.existing(&owner, prototype, revision)?
        {
            return Ok(existing);
        }
        if self.cached(|all| owned(all, &owner).any(Delivery::unresolved))? {
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
            resolution: None,
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
        self.writable()?;
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

    /// Positive provider evidence that the instruction arrived: a correlated reply or a scoped
    /// acknowledgment.
    ///
    /// It also overturns the user's own "it was not delivered", because that was a judgment made
    /// without this evidence and keeping it would leave the record wrong about what the model has.
    /// It never overturns a withdrawal: nothing collected a withdrawn instruction.
    pub fn confirm(&self, id: &str, owner: &Owner) -> Result<Delivery, String> {
        let _guard = self.mutation.lock();
        self.writable()?;
        let mut delivery = self.read(id)?;
        if delivery.owner != *owner {
            return Err("Delivery belongs to another conversation".into());
        }
        match (delivery.status, delivery.resolution) {
            (DeliveryStatus::Delivered, _) => return Ok(delivery),
            (DeliveryStatus::Unknown, _)
            | (DeliveryStatus::Rejected, Some(Resolution::NotDelivered)) => {}
            _ => return Err("Delivery has already changed; it will not be replayed".into()),
        }
        delivery.status = DeliveryStatus::Delivered;
        delivery.error = None;
        delivery.resolution = None;
        self.save(&delivery)?;
        Ok(delivery)
    }

    /// The user's own answer for a receipt no provider evidence will settle. Nothing is replayed:
    /// a rejected receipt only means a later, explicit Implement may prepare a new one.
    ///
    /// `owns` decides whether the asking pane may answer for this receipt's conversation.
    pub fn resolve(
        &self,
        id: &str,
        owns: impl FnOnce(&Owner) -> bool,
        resolution: Resolution,
    ) -> Result<Delivery, String> {
        let _guard = self.mutation.lock();
        self.writable()?;
        let mut delivery = self.read(id)?;
        if !owns(&delivery.owner) {
            return Err("Delivery belongs to another conversation".into());
        }
        let (from, reason) = match resolution {
            Resolution::NotDelivered => (DeliveryStatus::Unknown, NOT_DELIVERED),
            Resolution::Withdrawn => (DeliveryStatus::Pending, WITHDRAWN),
        };
        if delivery.status != from {
            return Err("Delivery has already changed; refresh before resolving it".into());
        }
        delivery.status = DeliveryStatus::Rejected;
        delivery.error = Some(reason.to_owned());
        delivery.resolution = Some(resolution);
        self.save(&delivery)?;
        Ok(delivery)
    }

    pub fn read(&self, id: &str) -> Result<Delivery, String> {
        let id = uuid::Uuid::parse_str(id).map_err(|_| "Invalid receipt")?;
        self.cached(|all| all.get(&id.to_string()).cloned())?
            .ok_or_else(|| "Unknown receipt".to_owned())
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
        self.writable()?;
        let candidates = self.cached(|all| {
            owned(all, owner)
                .filter(|d| {
                    d.status == DeliveryStatus::Unknown
                        || d.resolution == Some(Resolution::NotDelivered)
                })
                .cloned()
                .collect::<Vec<_>>()
        })?;
        let mut changed = false;
        for mut delivery in candidates {
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
                delivery.resolution = None;
                self.save(&delivery)?;
                changed = true;
            }
        }
        Ok(changed)
    }

    /// Delete delivered and rejected receipts last written more than [`SETTLED_RETENTION`] before
    /// `now_unix_ms`. Returns how many went.
    ///
    /// The file's own modification time is the settle time: a settled receipt is not written
    /// again unless provider evidence overturns a user's answer, which restarts its clock fairly.
    /// Reading it there spares every receipt a timestamp it would only carry for this.
    pub fn prune(&self, now_unix_ms: u64) -> usize {
        let _guard = self.mutation.lock();
        let now = std::time::UNIX_EPOCH + Duration::from_millis(now_unix_ms);
        let Some(cutoff) = now.checked_sub(SETTLED_RETENTION) else {
            return 0;
        };
        let settled = self
            .cached(|all| {
                all.values()
                    .filter(|d| !d.unresolved())
                    .map(|d| d.id.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let expired = settled
            .into_iter()
            .filter(|id| {
                std::fs::symlink_metadata(self.path(id))
                    .and_then(|m| m.modified())
                    .is_ok_and(|modified| modified < cutoff)
            })
            .collect::<Vec<_>>();
        self.remove(&expired)
    }

    /// Forget every receipt for a removed worktree, unresolved ones included: the path they name is
    /// gone, so no pane can show them again or act on them. Returns how many went.
    pub fn forget_worktree(&self, worktree: &str) -> usize {
        let _guard = self.mutation.lock();
        let gone = self
            .cached(|all| {
                all.values()
                    .filter(|d| d.owner.worktree == worktree)
                    .map(|d| d.id.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        self.remove(&gone)
    }

    /// Callers hold `mutation`.
    fn remove(&self, ids: &[String]) -> usize {
        let mut removed = 0;
        for id in ids {
            match std::fs::remove_file(self.path(id)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                // Still on disk means still in the journal the next process reads, so the
                // memory copy stays too rather than disagreeing with it.
                Err(_) => continue,
            }
            if let Journal::Loaded(all) = &mut *self.journal.lock() {
                all.remove(id);
            }
            removed += 1;
        }
        removed
    }

    fn cached<T>(&self, read: impl FnOnce(&Receipts) -> T) -> Result<T, String> {
        match &*self.journal.lock() {
            Journal::Loaded(all) => Ok(read(all)),
            Journal::Unreadable(error) => Err(error.clone()),
        }
    }

    /// Writers retry a journal that could not be read, so a transient failure at startup does not
    /// block every later instruction. Callers hold `mutation`.
    fn writable(&self) -> Result<(), String> {
        if matches!(&*self.journal.lock(), Journal::Loaded(_)) {
            return Ok(());
        }
        let journal = load(&self.root);
        let result = match &journal {
            Journal::Loaded(_) => Ok(()),
            Journal::Unreadable(error) => Err(error.clone()),
        };
        *self.journal.lock() = journal;
        result
    }

    fn path(&self, id: &str) -> PathBuf {
        self.root.join(format!("{id}.json"))
    }

    /// Callers hold `mutation`; the memory copy changes only once the file is durable.
    fn save(&self, delivery: &Delivery) -> Result<(), String> {
        private_dir(&self.root, "The native delivery journal")?;
        let bytes = serde_json::to_vec(delivery).map_err(|e| e.to_string())?;
        // The app's one durable writer: the receipt is complete or absent after a crash, and a
        // failed rename does not leave a temporary behind for `load` to skip forever.
        crate::app::write_private_atomically(&self.path(&delivery.id), &bytes)
            .map_err(|e| format!("The native delivery journal could not be written: {e}"))?;
        if let Journal::Loaded(all) = &mut *self.journal.lock() {
            all.insert(delivery.id.clone(), delivery.clone());
        }
        Ok(())
    }
}

fn owned<'a>(all: &'a Receipts, owner: &'a Owner) -> impl Iterator<Item = &'a Delivery> + 'a {
    all.values().filter(move |d| d.owner == *owner)
}

fn load(root: &Path) -> Journal {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Journal::Loaded(BTreeMap::new());
        }
        Err(error) => return Journal::Unreadable(unverifiable(&error.to_string())),
    };
    let mut all = BTreeMap::new();
    for entry in entries {
        let path = match entry {
            Ok(entry) => entry.path(),
            Err(error) => return Journal::Unreadable(unverifiable(&error.to_string())),
        };
        if path.extension().and_then(|p| p.to_str()) != Some("json") {
            continue;
        }
        match read_file(&path) {
            Ok(delivery) => {
                all.insert(delivery.id.clone(), delivery);
            }
            Err(error) => return Journal::Unreadable(unverifiable(&error)),
        }
    }
    Journal::Loaded(all)
}

fn unverifiable(error: &str) -> String {
    format!("Delivery journal cannot be verified: {error}. Nothing will be sent.")
}

fn read_file(path: &Path) -> Result<Delivery, String> {
    let id = path
        .file_stem()
        .and_then(|p| p.to_str())
        .and_then(|p| uuid::Uuid::parse_str(p).ok())
        .ok_or("Invalid receipt filename")?;
    if std::fs::symlink_metadata(path).is_ok_and(|m| !m.is_file() || m.file_type().is_symlink()) {
        return Err("Linked receipt records are excluded".into());
    }
    let mut text = String::new();
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(512_000)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    let delivery: Delivery = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    // Keyed by id in memory, so a record copied onto another receipt's name would otherwise
    // answer for that receipt.
    if delivery.id != id.to_string() {
        return Err("Receipt record does not match its filename".into());
    }
    Ok(delivery)
}

/// A directory private to this user, named `what` in its errors.
///
/// Not `prototypes::private_dir`, whose errors name prototype storage: a failure creating the
/// journal or a session's socket directory has to say which, or the user looks in the wrong place.
pub(super) fn private_dir(path: &Path, what: &str) -> Result<(), String> {
    use std::os::unix::fs::DirBuilderExt;
    if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink() || !m.is_dir()) {
        return Err(format!("{what} must be a directory, not a link"));
    }
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(|e| format!("{what} could not be created: {e}"))
}
