//! Reviewed package snapshots, not live code loaded into the privileged webview.
//!
//! A development link records its source but executes a reviewed copy. Otherwise an editor save
//! between approval and spawn could change what was approved. Native commands remain trusted code,
//! not an OS sandbox; the digest is an integrity check, not a claim about an author's intentions.
//!
//! # Safe mode is an overlay
//!
//! `wtm --safe-mode`, and a registry that cannot be read, start with every user package off, every
//! region and built-in restored and the default experience selected. That is a view of the
//! registry, never a rewrite of it: the persisted file keeps the user's packages, bindings and
//! choices, so the next normal launch gets them back. An earlier version applied safe mode to the
//! registry in memory and the next unrelated change saved the wiped copy, so one launch in safe
//! mode quietly uninstalled a configuration. Changes made during safe mode are saved to the real
//! registry and take effect at once; the overlay stops masking exactly the things the user changed.
pub mod catalog;
pub mod contributions;
pub mod document;
pub mod extensions;
pub mod manifest;
pub mod session;
use contributions::{BUILTINS, Binding, Slot};
use manifest::Manifest;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use wtm_core::ports::exec::{CancelToken, CommandRunner, Invocation};

/// A package is hashed whenever it is activated and read in full by its reviewer, so it has to
/// stay small enough for both. The caps also bound what a hostile source can make preparation
/// walk and copy: a million-file tree stops at the entry limit, not at the disk.
const MAX_BYTES: u64 = 16 * 1024 * 1024;
const MAX_FILES: usize = 1024;
/// Each review pins a copied snapshot on disk until it is installed or cancelled.
const MAX_PENDING: usize = 16;
/// A clone of a package-sized repository over HTTPS.
const GIT_DEADLINE_MS: u64 = 120_000;
const COMMAND_DEADLINE_MS: u64 = 30_000;
const MAX_RESPONSE: usize = 1024 * 1024;
/// The activation runtime lives as long as its package is enabled, and `PipeHost` has no wait, so
/// this deadline is inert (see the `pipe` port's module docs). A week rather than `u64::MAX` for
/// the reason the terminal dock's `SHELL_TIMEOUT_MS` gives: `Instant + Duration` panics on overflow
/// for whoever first adds a wait.
const RUNTIME_DEADLINE_MS: u64 = 7 * 24 * 60 * 60 * 1_000;
const DEFAULT_EXPERIENCE: &str = "wtm.chat";
/// The value of an `extension_choices` entry that keeps WTM's own contribution.
const BUILTIN_CHOICE: &str = "builtin";
/// The prefix of an `extension_choices` key that chooses between styles for one semantic part.
const STYLE_CHOICE: &str = "style:";
/// Reviews in preparation or awaiting a decision. The registry never refers to anything here, so
/// whatever is left at startup belongs to a review that quitting abandoned.
const PENDING_DIR: &str = "pending";
const PACKAGES_DIR: &str = "packages";
const MANIFEST_FILE: &str = "wtm-plugin.toml";

const NOT_INSTALLED: &str = "Plugin is not installed";
const UNREADABLE: &str =
    "The plugin registry is unreadable. Recovery preserves it before resetting plugin settings.";
const BUSY: &str = "Detach this plugin's sessions and wait for its commands before installing or \
    rolling back a version";
const STALE: &str =
    "The plugin was disabled or changed while this request ran; its result was discarded";
const CHANGED: &str = "Package content changed since review. Reload and approve it again.";
const CANCELLED: &str = "Review cancelled";
const TOO_MANY: &str = "Too many pending reviews; cancel an earlier review first";
const GIT_SOURCE: &str = "Use an HTTPS Git URL without embedded credentials, query or fragment";
const NOT_SELF_CONTAINED: &str = "must be self-contained HTML. Inline its scripts and styles and \
    use data: URLs for images and fonts; external src, href, srcset or url() references, \
    @import, iframe, object, embed and base elements, fetch, XMLHttpRequest, WebSocket and \
    import() are refused.";
const NOT_A_FILE: &str =
    "Plugin packages may contain only regular files and directories, not symlinks or devices";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Package {
    pub manifest: Manifest,
    pub source: String,
    pub kind: String,
    pub revision: String,
    /// The tag, branch or commit as the user typed it, which **Review update** offers again.
    /// `revision` is the commit it resolved to at review; `None` for a local folder.
    #[serde(default)]
    pub requested: Option<String>,
    pub digest: String,
    pub directory: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Installed {
    pub current: Package,
    pub previous: Option<Package>,
    pub enabled: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Registry {
    pub version: u32,
    pub installed: BTreeMap<String, Installed>,
    pub disabled: BTreeSet<String>,
    pub experience: String,
    /// Whether this launch is in safe mode. Always false on disk: earlier builds saved it, so it
    /// is still read, and then ignored, because safe mode now lasts one launch.
    pub safe_mode: bool,
    pub bindings: BTreeMap<Slot, Binding>,
    pub extensions: BTreeMap<String, extensions::Preferences>,
    pub extension_choices: BTreeMap<String, String>,
    pub extension_order: Vec<String>,
}
impl Default for Registry {
    fn default() -> Self {
        Self {
            version: 1,
            installed: BTreeMap::new(),
            disabled: BTreeSet::new(),
            experience: DEFAULT_EXPERIENCE.into(),
            safe_mode: false,
            bindings: BTreeMap::new(),
            extensions: BTreeMap::new(),
            extension_choices: BTreeMap::new(),
            extension_order: Vec::new(),
        }
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct Review {
    pub token: String,
    pub package: Package,
    /// The digest of the installed snapshot this review would replace, if any. Equal to
    /// `package.digest` when the source has not changed; installing that is refused.
    pub replacing: Option<String>,
    /// [`Manifest::executable`], so the review shows the native-code warning by the host's rule.
    pub executable: bool,
}
#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    /// The registry as the app behaves now, with the safe-mode overlay applied.
    pub registry: Registry,
    pub safe_mode: bool,
    pub problem: Option<String>,
    pub builtins: &'static [contributions::Builtin],
}

/// Something the user changed during safe mode, which the overlay then stops masking.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Touch {
    Package(String),
    Builtin(String),
    Slot(Slot),
    Experience,
    Choice(String),
}

struct Inner {
    /// What is on disk. Mutations change this; reads go through the overlay.
    registry: Registry,
    safe_mode: bool,
    touched: BTreeSet<Touch>,
    problem: Option<String>,
    pending: BTreeMap<String, Review>,
    preparing: BTreeMap<String, CancelToken>,
    running: BTreeMap<String, usize>,
    attached: BTreeSet<(String, String, String)>,
    view_requests: BTreeMap<String, usize>,
    /// Package snapshots hashed since this process started, by plugin. Requests trust this; the
    /// activation boundaries (install, enable, runtime start, view open) re-hash and refresh it.
    verified: BTreeMap<String, String>,
}
pub struct Store {
    root: PathBuf,
    runner: Arc<dyn CommandRunner>,
    inner: Mutex<Inner>,
}
impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginStore")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}
fn io(e: impl std::fmt::Display) -> String {
    e.to_string()
}

impl Inner {
    fn masked(&self, touch: &Touch) -> bool {
        self.safe_mode && !self.touched.contains(touch)
    }
    fn package_enabled(&self, id: &str) -> bool {
        self.registry.installed.get(id).is_some_and(|p| p.enabled)
            && !self.masked(&Touch::Package(id.into()))
    }
    fn builtin_enabled(&self, id: &str) -> bool {
        !self.registry.disabled.contains(id) || self.masked(&Touch::Builtin(id.into()))
    }
    fn effective(&self) -> Registry {
        let mut r = self.registry.clone();
        r.safe_mode = self.safe_mode;
        if !self.safe_mode {
            return r;
        }
        let touched = |t: Touch| self.touched.contains(&t);
        r.disabled.retain(|id| touched(Touch::Builtin(id.clone())));
        if !touched(Touch::Experience) {
            r.experience = DEFAULT_EXPERIENCE.into();
        }
        for (id, p) in &mut r.installed {
            p.enabled &= touched(Touch::Package(id.clone()));
        }
        r.bindings.retain(|slot, _| touched(Touch::Slot(*slot)));
        // The WTM default is kept, as recovery keeps it: it can only ever select WTM's own UI.
        r.extension_choices.retain(|target, owner| {
            owner == BUILTIN_CHOICE || touched(Touch::Choice(target.clone()))
        });
        r
    }
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            registry: self.effective(),
            safe_mode: self.safe_mode,
            problem: self.problem.clone(),
            builtins: BUILTINS,
        }
    }
    /// A package's local session attachments, and its verification, end here. Disable, uninstall
    /// and recovery call this before they change anything and are never refused for attachments
    /// or running calls: they are how a user gets away from a package that misbehaves.
    fn revoke(&mut self, id: &str) {
        self.attached.retain(|(p, _, _)| p != id);
        self.verified.remove(id);
    }
}

impl Store {
    pub fn new(root: PathBuf, runner: Arc<dyn CommandRunner>, safe_mode: bool) -> Self {
        let loaded = std::fs::read(root.join("registry.json"));
        let (mut registry, problem) = match loaded {
            Ok(bytes) => match serde_json::from_slice::<Registry>(&bytes) {
                Ok(r) if r.version == 1 => (r, None),
                _ => (Registry::default(), Some(UNREADABLE.into())),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Registry::default(), None),
            Err(e) => (Registry::default(), Some(e.to_string())),
        };
        registry.safe_mode = false;
        let _ = std::fs::remove_dir_all(root.join(PENDING_DIR));
        Self {
            root,
            runner,
            inner: Mutex::new(Inner {
                registry,
                safe_mode: safe_mode || problem.is_some(),
                touched: BTreeSet::new(),
                problem,
                pending: BTreeMap::new(),
                preparing: BTreeMap::new(),
                running: BTreeMap::new(),
                attached: BTreeSet::new(),
                view_requests: BTreeMap::new(),
                verified: BTreeMap::new(),
            }),
        }
    }
    pub fn snapshot(&self) -> Snapshot {
        self.inner.lock().snapshot()
    }
    pub fn enabled(&self, id: &str) -> bool {
        let i = self.inner.lock();
        if BUILTINS.iter().any(|b| b.id == id) {
            i.builtin_enabled(id)
        } else {
            i.package_enabled(id)
        }
    }
    /// Save a change to the persisted registry, then let the overlay show it.
    ///
    /// In safe mode the change is first checked against the registry the user is looking at, so a
    /// dependency rule the overlay satisfies (Home needs WTM Chat, the default experience must be
    /// enabled) cannot be broken through the persisted state underneath it, nor the reverse.
    fn apply(
        &self,
        i: &mut Inner,
        touches: impl IntoIterator<Item = Touch>,
        f: impl Fn(&mut Registry) -> Result<(), String>,
    ) -> Result<Snapshot, String> {
        if let Some(problem) = &i.problem {
            return Err(problem.clone());
        }
        if i.safe_mode {
            f(&mut i.effective())?;
        }
        let mut next = i.registry.clone();
        f(&mut next)?;
        write_atomically(
            &self.root.join("registry.json"),
            &serde_json::to_vec_pretty(&next).map_err(io)?,
        )?;
        i.registry = next;
        if i.safe_mode {
            i.touched.extend(touches);
        }
        Ok(i.snapshot())
    }
    fn mutate(
        &self,
        touches: impl IntoIterator<Item = Touch>,
        f: impl Fn(&mut Registry) -> Result<(), String>,
    ) -> Result<Snapshot, String> {
        self.apply(&mut self.inner.lock(), touches, f)
    }
    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<Snapshot, String> {
        let builtin = BUILTINS.iter().any(|b| b.id == id);
        let mut i = self.inner.lock();
        if !builtin {
            if enabled {
                let package = i
                    .registry
                    .installed
                    .get(id)
                    .ok_or(NOT_INSTALLED)?
                    .current
                    .clone();
                self.verify(&package)?;
                i.verified.insert(id.into(), package.directory);
            } else {
                i.revoke(id);
            }
        }
        let touch = if builtin {
            Touch::Builtin(id.into())
        } else {
            Touch::Package(id.into())
        };
        self.apply(&mut i, [touch], |r| {
            if !enabled && id == "wtm.chat" && !r.disabled.contains("wtm.home") {
                return Err("Disable Home before disabling its WTM Chat dependency".into());
            }
            if enabled && id == "wtm.home" && r.disabled.contains("wtm.chat") {
                return Err("Enable WTM Chat before enabling Home".into());
            }
            if !enabled && r.experience == id {
                return Err("Choose another default experience first".into());
            }
            if builtin {
                if enabled {
                    r.disabled.remove(id);
                } else {
                    r.disabled.insert(id.into());
                }
            } else {
                r.installed.get_mut(id).ok_or(NOT_INSTALLED)?.enabled = enabled;
                if !enabled {
                    forget_contributions(r, id);
                }
            }
            Ok(())
        })
    }
    pub fn experience(&self, id: &str) -> Result<Snapshot, String> {
        self.mutate([Touch::Experience], |r| {
            if !["wtm.chat", "wtm.native-cli"].contains(&id) || r.disabled.contains(id) {
                return Err("Choose an enabled agent experience".into());
            }
            r.experience = id.into();
            Ok(())
        })
    }
    /// Turn every user package off and restore every region, saved, and enter safe mode.
    ///
    /// Never refused for running calls or attached sessions: it revokes them instead. Choices of a
    /// plugin's contribution go and choices of WTM's default stay, as safe mode's overlay has it.
    pub fn recover(&self) -> Result<Snapshot, String> {
        let mut i = self.inner.lock();
        let file = self.root.join("registry.json");
        if i.problem.is_some() && file.exists() {
            let kept = format!("registry-corrupt-{}.json", uuid::Uuid::new_v4());
            std::fs::rename(&file, self.root.join(kept)).map_err(io)?;
        }
        let mut next = i.registry.clone();
        next.bindings.clear();
        next.extension_choices
            .retain(|_, owner| owner == BUILTIN_CHOICE);
        next.disabled.clear();
        next.experience = DEFAULT_EXPERIENCE.into();
        for p in next.installed.values_mut() {
            p.enabled = false;
        }
        write_atomically(&file, &serde_json::to_vec_pretty(&next).map_err(io)?)?;
        i.registry = next;
        i.problem = None;
        i.attached.clear();
        i.verified.clear();
        i.safe_mode = true;
        i.touched.clear();
        Ok(i.snapshot())
    }
    pub fn prepare(&self, source: &str, kind: &str, revision: &str) -> Result<Review, String> {
        self.prepare_as(source, kind, revision, None)
    }
    /// [`Store::prepare`] under a token the caller chose, so [`Store::cancel`] can stop it while
    /// Git is still fetching rather than only after the review exists.
    pub fn prepare_as(
        &self,
        source: &str,
        kind: &str,
        revision: &str,
        token: Option<&str>,
    ) -> Result<Review, String> {
        let git = match kind {
            "git" => true,
            "local" => false,
            _ => return Err("Choose a local folder or Git repository".into()),
        };
        let requested = revision.trim();
        if git {
            check_git_source(source)?;
            if requested.is_empty() {
                return Err("Choose an explicit tag, branch or commit".into());
            }
        }
        let token = match token {
            Some(t) => uuid::Uuid::parse_str(t)
                .map_err(|_| "Invalid review token")?
                .to_string(),
            None => uuid::Uuid::new_v4().to_string(),
        };
        let cancel = CancelToken::new();
        {
            let mut i = self.inner.lock();
            if i.pending.len() + i.preparing.len() >= MAX_PENDING {
                return Err(TOO_MANY.into());
            }
            if i.pending.contains_key(&token) || i.preparing.contains_key(&token) {
                return Err("This review is already in progress".into());
            }
            i.preparing.insert(token.clone(), cancel.clone());
        }
        let directory = self.root.join(PENDING_DIR).join(&token);
        let prepared = self
            .copy_source(&directory, source, git.then_some(requested), &cancel)
            .and_then(|(source, revision)| {
                let digest = fingerprint(&directory)?;
                let manifest = Manifest::parse(&read(&directory.join(MANIFEST_FILE))?)?;
                check_documents(&directory, &manifest)?;
                Ok(Package {
                    manifest,
                    source,
                    kind: kind.into(),
                    revision,
                    requested: git.then(|| requested.to_owned()),
                    digest,
                    directory: token.clone(),
                })
            });
        let mut i = self.inner.lock();
        i.preparing.remove(&token);
        // Checked under the lock that publishes the review, so a cancel racing the last step wins,
        // and a Git failure caused by the cancel reads as the cancel.
        let review = if cancel.is_cancelled() {
            Err(CANCELLED.into())
        } else {
            prepared.map(|package| Review {
                token: token.clone(),
                replacing: i
                    .registry
                    .installed
                    .get(&package.manifest.id)
                    .map(|p| p.current.digest.clone()),
                executable: package.manifest.executable(),
                package,
            })
        };
        if let Ok(review) = &review {
            i.pending.insert(token, review.clone());
        } else {
            drop(i);
            let _ = std::fs::remove_dir_all(&directory);
        }
        review
    }
    fn copy_source(
        &self,
        directory: &Path,
        source: &str,
        revision: Option<&str>,
        cancel: &CancelToken,
    ) -> Result<(String, String), String> {
        std::fs::create_dir_all(directory).map_err(io)?;
        if let Some(revision) = revision {
            let commit = self.clone_revision(directory, source, revision, cancel)?;
            return Ok((source.to_owned(), commit));
        }
        if !Path::new(source).is_absolute() {
            return Err("Use an absolute local folder path".into());
        }
        let path = Path::new(source).canonicalize().map_err(io)?;
        if self.root.starts_with(&path) || path.starts_with(&self.root) {
            return Err("A plugin source cannot contain wtm's plugin storage".into());
        }
        for (name, _) in files(&path, true)? {
            if cancel.is_cancelled() {
                return Err(CANCELLED.into());
            }
            let target = directory.join(&name);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(io)?;
            }
            std::fs::copy(path.join(name), target).map_err(io)?;
        }
        Ok((
            path.to_string_lossy().into_owned(),
            "Development snapshot".into(),
        ))
    }
    fn clone_revision(
        &self,
        directory: &Path,
        source: &str,
        revision: &str,
        cancel: &CancelToken,
    ) -> Result<String, String> {
        let target = directory.to_str().ok_or("Invalid package path")?;
        // An empty template: no sample hooks or info files are copied in from the Git install.
        let clone = [
            "clone",
            "--no-checkout",
            "--template=",
            "--",
            source,
            target,
        ];
        self.git(&self.root, &clone, cancel)?;
        let spec = format!("{revision}^{{commit}}");
        let resolve = ["rev-parse", "--verify", "--end-of-options", spec.as_str()];
        let commit = self.git(directory, &resolve, cancel)?.trim().to_owned();
        if !commit.bytes().all(|b| b.is_ascii_hexdigit()) || commit.len() != 40 {
            return Err("Git returned an invalid commit".into());
        }
        self.git(
            directory,
            &["checkout", "--detach", "--force", &commit],
            cancel,
        )?;
        // The snapshot is a tree, not a repository. A `.git` left in it would carry Git's config
        // into a directory WTM later runs programs from, and it is nothing the reviewer saw.
        std::fs::remove_dir_all(directory.join(".git")).map_err(io)?;
        Ok(commit)
    }
    fn git(&self, cwd: &Path, arguments: &[&str], cancel: &CancelToken) -> Result<String, String> {
        // No hooks, and no transport but HTTPS: a repository's submodule or remote cannot switch
        // to `ext::` (which runs a program), `file://` (which reads a local path) or plain HTTP.
        let mut argv: Vec<String> = [
            "git",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "protocol.allow=never",
            "-c",
            "protocol.https.allow=always",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        argv.extend(arguments.iter().map(|s| (*s).into()));
        let mut inv = Invocation::new(argv, cwd, GIT_DEADLINE_MS);
        // The user's and the system's Git configuration are left out, so an `insteadOf`, an
        // `include` or a credential helper cannot change what preparation fetches or runs. That
        // has a deliberate cost: with no credential helper nothing can authenticate, so only
        // public HTTPS repositories can be prepared, and `GIT_TERMINAL_PROMPT=0` makes a private
        // one fail at once rather than wait for a password no terminal will ever ask for.
        inv.env = BTreeMap::from([
            ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
            ("GIT_CONFIG_SYSTEM".into(), "/dev/null".into()),
            ("GIT_CONFIG_COUNT".into(), "0".into()),
            ("GIT_TERMINAL_PROMPT".into(), "0".into()),
        ]);
        self.runner.run(&inv, cancel).map(|o| o.stdout).map_err(io)
    }
    /// Stop a preparation in flight, or discard a finished review. Either way its copy goes.
    pub fn cancel(&self, token: &str) -> Result<(), String> {
        let mut i = self.inner.lock();
        if let Some(cancel) = i.preparing.get(token) {
            // The preparing thread sees this, removes its own directory and reports the cancel.
            cancel.cancel();
            return Ok(());
        }
        let review = i.pending.remove(token);
        drop(i);
        if let Some(review) = review {
            let dir = self.place(&review.package.directory, PENDING_DIR)?;
            std::fs::remove_dir_all(dir).map_err(io)?;
        }
        Ok(())
    }
    /// A review awaiting a decision, so the host can stop the right runtime before installing it.
    pub fn review(&self, token: &str) -> Option<Review> {
        self.inner.lock().pending.get(token).cloned()
    }
    pub fn install(&self, token: &str) -> Result<Snapshot, String> {
        let mut i = self.inner.lock();
        if let Some(problem) = &i.problem {
            return Err(problem.clone());
        }
        let review = i
            .pending
            .get(token)
            .ok_or("Review expired; review the source again")?
            .clone();
        let id = review.package.manifest.id.clone();
        ensure_idle(&i, &id)?;
        let current = i.registry.installed.get(&id).map(|p| p.current.clone());
        if current.as_ref().map(|p| &p.digest) != review.replacing.as_ref() {
            return Err("The installed version changed during review; review again".into());
        }
        // Installing the same bytes again would make the current snapshot its own rollback
        // target and drop the real previous version, so it is refused rather than done.
        if current
            .as_ref()
            .is_some_and(|p| p.digest == review.package.digest)
        {
            return Err("This revision is already installed".into());
        }
        let pending = self.place(&review.package.directory, PENDING_DIR)?;
        let installed = self.directory(&review.package)?;
        std::fs::create_dir_all(self.root.join(PACKAGES_DIR)).map_err(io)?;
        std::fs::rename(&pending, &installed).map_err(io)?;
        let result = self.verify(&review.package).and_then(|_| {
            self.apply(&mut i, [Touch::Package(id.clone())], |r| {
                r.installed.insert(
                    id.clone(),
                    Installed {
                        current: review.package.clone(),
                        previous: current.clone(),
                        enabled: true,
                    },
                );
                Ok(())
            })
        });
        if result.is_ok() {
            i.pending.remove(token);
            i.verified.insert(id, review.package.directory.clone());
        } else {
            let _ = std::fs::rename(&installed, &pending);
        }
        result
    }
    /// Remove a package from the registry. Its snapshots and data stay on disk.
    pub fn uninstall(&self, id: &str) -> Result<Snapshot, String> {
        let mut i = self.inner.lock();
        i.revoke(id);
        self.apply(&mut i, [Touch::Package(id.into())], |r| {
            r.installed.remove(id).ok_or(NOT_INSTALLED)?;
            forget_contributions(r, id);
            Ok(())
        })
    }
    pub fn rollback(&self, id: &str) -> Result<Snapshot, String> {
        let mut i = self.inner.lock();
        ensure_idle(&i, id)?;
        let previous = i
            .registry
            .installed
            .get(id)
            .ok_or(NOT_INSTALLED)?
            .previous
            .clone()
            .ok_or("No previous version is retained")?;
        self.verify(&previous)?;
        let snapshot = self.apply(&mut i, [], |r| {
            let p = r.installed.get_mut(id).ok_or(NOT_INSTALLED)?;
            let previous = p.previous.take().ok_or("No previous version is retained")?;
            p.previous = Some(std::mem::replace(&mut p.current, previous));
            Ok(())
        })?;
        i.verified.insert(id.into(), previous.directory);
        Ok(snapshot)
    }
    fn place(&self, directory: &str, place: &str) -> Result<PathBuf, String> {
        uuid::Uuid::parse_str(directory).map_err(io)?;
        Ok(self.root.join(place).join(directory))
    }
    fn directory(&self, p: &Package) -> Result<PathBuf, String> {
        self.place(&p.directory, PACKAGES_DIR)
    }
    fn verify(&self, p: &Package) -> Result<PathBuf, String> {
        let dir = self.directory(p)?;
        if fingerprint(&dir)? != p.digest {
            return Err(CHANGED.into());
        }
        // The digest covers the manifest file, but the host enforces the registry's parsed copy
        // of it: commands, access and views. A registry edited by hand to widen what a reviewed
        // package may do still matches the digest, so the two copies are compared as well.
        let actual = Manifest::parse(&read(&dir.join(MANIFEST_FILE))?)?;
        if actual != p.manifest {
            return Err("Stored manifest differs from reviewed package".into());
        }
        Ok(dir)
    }
    /// The enabled package, hashed once per process rather than on every request.
    ///
    /// Requests arrive many times a second from views and runtimes, and hashing up to 16 MiB for
    /// each bought nothing the activation boundaries do not: [`Store::activate`] re-hashes when
    /// code is about to start running from the snapshot.
    pub fn active(&self, id: &str) -> Result<(Package, PathBuf), String> {
        let (package, verified) = {
            let i = self.inner.lock();
            let p = i.registry.installed.get(id).ok_or(NOT_INSTALLED)?;
            if !i.package_enabled(id) {
                return Err(if i.safe_mode && p.enabled {
                    "User plugins are disabled in safe mode".into()
                } else {
                    "Plugin is disabled".into()
                });
            }
            let verified = i.verified.get(id) == Some(&p.current.directory);
            (p.current.clone(), verified)
        };
        if verified {
            let dir = self.directory(&package)?;
            return Ok((package, dir));
        }
        let dir = self.verify(&package)?;
        let mut i = self.inner.lock();
        // Remembered only if it is still the current snapshot; an install that raced this hash
        // has verified its own.
        if i.registry
            .installed
            .get(id)
            .is_some_and(|p| p.current.directory == package.directory)
        {
            i.verified.insert(id.into(), package.directory.clone());
        }
        Ok((package, dir))
    }
    /// [`Store::active`], re-hashing the snapshot first. For the points where code starts to run.
    pub fn activate(&self, id: &str) -> Result<(Package, PathBuf), String> {
        self.inner.lock().verified.remove(id);
        self.active(id)
    }
    /// A panel's document, for opening it. Re-hashes the package and checks the document.
    pub fn panel(&self, id: &str, contribution: &str) -> Result<String, String> {
        let (p, dir) = self.activate(id)?;
        let panel = p
            .manifest
            .panels
            .iter()
            .find(|p| p.id == contribution)
            .ok_or("Unknown panel")?;
        open_document(&dir, &panel.file)
    }
    /// Whether an open panel may still answer requests. Reads no files.
    pub fn panel_declared(&self, id: &str, contribution: &str) -> Result<(), String> {
        let (p, _) = self.active(id)?;
        if p.manifest.panels.iter().any(|p| p.id == contribution) {
            Ok(())
        } else {
            Err("Unknown panel".into())
        }
    }
    /// A terminal profile's argv alone. The app launches through
    /// [`Store::terminal_invocation`], which carries the package environment as well.
    pub fn terminal(&self, id: &str, contribution: &str) -> Result<Vec<String>, String> {
        self.terminal_invocation(id, contribution, Path::new("/"))
            .map(|inv| inv.argv)
    }
    pub fn terminal_invocation(
        &self,
        id: &str,
        contribution: &str,
        cwd: &Path,
    ) -> Result<Invocation, String> {
        let (package, dir) = self.activate(id)?;
        let entry = package
            .manifest
            .terminals
            .iter()
            .find(|c| c.id == contribution)
            .ok_or("Unknown terminal profile")?;
        self.launch(&package, &dir, &entry.command, cwd, 1, RUNTIME_DEADLINE_MS)
    }
    /// The one place a package program becomes an invocation, so a command, a terminal and a
    /// runtime cannot disagree about what `./tool` means or what environment they get.
    ///
    /// The working directory is the caller's: commands and terminals act on the worktree they
    /// were started from, because that is what `git status` in a manifest means. The runtime is
    /// one process per package and belongs to no worktree, so it runs in its own snapshot and
    /// reaches workspaces through host calls.
    fn launch(
        &self,
        package: &Package,
        dir: &Path,
        argv: &[String],
        cwd: &Path,
        api: u32,
        deadline_ms: u64,
    ) -> Result<Invocation, String> {
        let argv = resolve_argv(dir, argv)?;
        let data = self.root.join("data").join(&package.manifest.id);
        std::fs::create_dir_all(&data).map_err(io)?;
        Ok(
            Invocation::new(argv, cwd, deadline_ms).with_env(BTreeMap::from([
                ("WTM_PLUGIN_ROOT".into(), dir.to_string_lossy().into_owned()),
                (
                    "WTM_PLUGIN_DATA".into(),
                    data.to_string_lossy().into_owned(),
                ),
                ("WTM_PLUGIN_API".into(), api.to_string()),
            ])),
        )
    }
    pub fn invoke(
        &self,
        id: &str,
        contribution: &str,
        backend: bool,
        request: &str,
        cwd: &Path,
    ) -> Result<String, String> {
        let _running = self.activation(id);
        let (p, dir) = self.active(id)?;
        let commands = if backend {
            &p.manifest.session_backends
        } else {
            &p.manifest.commands
        };
        let command = commands
            .iter()
            .find(|c| c.id == contribution)
            .ok_or("Unknown contribution")?;
        let inv = self
            .launch(&p, &dir, &command.command, cwd, 1, COMMAND_DEADLINE_MS)?
            .with_stdin(request);
        let result = self.runner.run(&inv, &CancelToken::new()).map_err(io)?;
        // Disabling does not wait for running commands, so a command can outlive its package's
        // activation. Its answer is dropped rather than handed to a caller that was revoked.
        self.still_current(id, &p.directory)?;
        if result.stdout.len() > MAX_RESPONSE {
            return Err("Plugin response exceeds one MiB".into());
        }
        Ok(result.stdout)
    }
    fn still_current(&self, id: &str, directory: &str) -> Result<(), String> {
        let i = self.inner.lock();
        let same = i
            .registry
            .installed
            .get(id)
            .is_some_and(|p| p.current.directory == directory);
        if same && i.package_enabled(id) {
            Ok(())
        } else {
            Err(STALE.into())
        }
    }
}

/// `./program` is a file in the package; anything else is used as written, so a bare name is
/// looked up on `PATH` and an absolute path is that path. One rule for every entry point.
pub fn resolve_argv(dir: &Path, argv: &[String]) -> Result<Vec<String>, String> {
    let mut argv = argv.to_vec();
    let program = argv
        .first_mut()
        .ok_or("Commands must be nonempty argv arrays")?;
    if program.starts_with("./") {
        manifest::relative(program)?;
        *program = dir.join(&*program).to_string_lossy().into_owned();
    }
    Ok(argv)
}

fn forget_contributions(r: &mut Registry, id: &str) {
    let prefix = format!("{id}:");
    r.extension_choices
        .retain(|_, owner| !owner.starts_with(&prefix));
    r.bindings
        .retain(|_, b| !matches!(b, Binding::Plugin { plugin, .. } if plugin == id));
}

/// HTTPS, and nothing Git or the registry could read two ways.
///
/// `@` is how a URL carries credentials, which would then be saved in the registry and shown in
/// every review. Git builds its requests by appending paths to this string, so a `?` query or a
/// `#` fragment would change which URL is actually requested while the review shows the same one.
/// Line breaks are refused because Git's own config syntax would read one as a second setting.
fn check_git_source(source: &str) -> Result<(), String> {
    if !source.starts_with("https://")
        || source.contains('@')
        || source.contains(['\n', '\r', '?', '#'])
    {
        return Err(GIT_SOURCE.into());
    }
    Ok(())
}

fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(io)
}

/// Every panel and view must render with nothing beside it, checked when the package is prepared
/// so a package that cannot render is refused at review, while its author is still there.
fn check_documents(dir: &Path, manifest: &Manifest) -> Result<(), String> {
    let files = manifest.panels.iter().map(|p| &p.file);
    for file in files.chain(manifest.views.iter().map(|v| &v.file)) {
        if !dir.join(file).is_file() {
            return Err(format!("Missing view file: {file}"));
        }
        check_document(file, &read(&dir.join(file))?)?;
    }
    Ok(())
}

fn check_document(file: &str, html: &str) -> Result<(), String> {
    if document::self_contained(html) {
        Ok(())
    } else {
        Err(format!("{file} {NOT_SELF_CONTAINED}"))
    }
}

fn open_document(dir: &Path, file: &str) -> Result<String, String> {
    manifest::relative(file)?;
    let html = read(&dir.join(file))?;
    check_document(file, &html)?;
    Ok(html)
}

/// Replace `path` so that a crash leaves the old file or the new one, never a torn one.
///
/// The data is synced before the rename and the directory after it: without the first a crash can
/// leave the new name pointing at blocks never written, and without the second the rename itself
/// may not have reached the disk, so the next launch reads the registry from before the change.
/// `wtm-config` has the same pattern for its own files but no shared helper, and this crate does
/// not depend on it.
fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let dir = path.parent().ok_or("A registry path needs a directory")?;
    std::fs::create_dir_all(dir).map_err(io)?;
    let name = path
        .file_name()
        .map_or_else(|| "registry".into(), |n| n.to_string_lossy().into_owned());
    let temp = dir.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4()));
    let written = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temp, path)?;
        std::fs::File::open(dir)?.sync_all()
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written.map_err(io)
}

/// The regular files under `root`, with their sizes, refusing anything else.
///
/// `skip_git` leaves out a `.git` at the root only, which is the source's own repository when a
/// development folder is copied. The snapshot is always walked with it off, so nothing in a
/// reviewed package, at any depth, is left out of its digest.
fn files(root: &Path, skip_git: bool) -> Result<Vec<(PathBuf, u64)>, String> {
    if !root.symlink_metadata().map_err(io)?.is_dir() {
        return Err("Package root must be a real directory".into());
    }
    let mut entries = 0;
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    let mut total = 0;
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).map_err(io)? {
            let entry = entry.map_err(io)?;
            if skip_git && dir == root && entry.file_name() == ".git" {
                continue;
            }
            entries += 1;
            if entries > MAX_FILES {
                return Err("Package exceeds 1024 entries".into());
            }
            let meta = entry.path().symlink_metadata().map_err(io)?;
            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() {
                total += meta.len();
                out.push((
                    entry.path().strip_prefix(root).map_err(io)?.to_path_buf(),
                    meta.len(),
                ));
            } else {
                return Err(NOT_A_FILE.into());
            }
            if out.len() + stack.len() > MAX_FILES || total > MAX_BYTES {
                return Err("Package exceeds 1024 entries or 16 MiB".into());
            }
        }
    }
    out.sort();
    Ok(out)
}
fn fingerprint(root: &Path) -> Result<String, String> {
    let mut hash = Sha256::new();
    for (path, _) in files(root, false)? {
        let name = path.to_string_lossy();
        hash.update((name.len() as u64).to_le_bytes());
        hash.update(name.as_bytes());
        let data = std::fs::read(root.join(&path)).map_err(io)?;
        hash.update((data.len() as u64).to_le_bytes());
        hash.update(data);
    }
    let mut digest = String::with_capacity(64);
    for byte in hash.finalize() {
        write!(&mut digest, "{byte:02x}").map_err(io)?;
    }
    Ok(digest)
}

/// Installing or rolling back replaces the code a running call is executing, so those two wait.
/// Disable, uninstall and recovery do not: see [`Inner::revoke`].
fn ensure_idle(inner: &Inner, id: &str) -> Result<(), String> {
    if inner.running.get(id).is_some_and(|n| *n > 0)
        || inner.attached.iter().any(|(p, _, _)| p == id)
    {
        return Err(BUSY.into());
    }
    Ok(())
}
#[derive(Debug)]
pub struct Activation<'a> {
    store: &'a Store,
    plugin: String,
}
impl Drop for Activation<'_> {
    fn drop(&mut self) {
        let mut i = self.store.inner.lock();
        if let Some(n) = i.running.get_mut(&self.plugin) {
            *n = n.saturating_sub(1);
        }
    }
}
impl Store {
    pub fn try_view_request(&self, plugin: &str) -> bool {
        let mut i = self.inner.lock();
        let count = i.view_requests.entry(plugin.into()).or_default();
        if *count >= 16 {
            return false;
        }
        *count += 1;
        true
    }
    pub fn finish_view_request(&self, plugin: &str) {
        if let Some(count) = self.inner.lock().view_requests.get_mut(plugin) {
            *count = count.saturating_sub(1);
        }
    }
    pub fn release_client(&self, client: &str) {
        self.inner.lock().attached.retain(|(_, _, c)| c != client);
    }
    /// Held while a call runs package code, so installing or rolling back waits for it.
    pub fn activation(&self, id: &str) -> Activation<'_> {
        *self.inner.lock().running.entry(id.into()).or_default() += 1;
        Activation {
            store: self,
            plugin: id.into(),
        }
    }
    /// Record a session attachment, unless the package was revoked while the attach ran.
    fn attach(&self, key: (String, String, String)) -> Result<(), String> {
        let mut i = self.inner.lock();
        if !i.package_enabled(&key.0) {
            return Err(STALE.into());
        }
        i.attached.insert(key);
        Ok(())
    }

    pub fn bind(&self, slot: Slot, binding: Binding) -> Result<Snapshot, String> {
        let mut i = self.inner.lock();
        if let Binding::Plugin { plugin, .. } = &binding {
            // Selecting a view is an activation boundary: what it will run is hashed now.
            let package = i
                .registry
                .installed
                .get(plugin)
                .ok_or(NOT_INSTALLED)?
                .current
                .clone();
            self.verify(&package)?;
            i.verified.insert(plugin.clone(), package.directory);
        }
        let mut touches = vec![Touch::Slot(slot)];
        if binding == Binding::Builtin {
            touches.push(Touch::Builtin(slot.builtin().id.into()));
        }
        self.apply(&mut i, touches, move |r| {
            if binding == Binding::Builtin {
                if slot == Slot::Home && r.disabled.contains("wtm.chat") {
                    return Err("Enable WTM Chat before enabling Home".into());
                }
                r.disabled.remove(slot.builtin().id);
            }
            if let Binding::Plugin { plugin, view } = &binding {
                let p = r.installed.get(plugin).ok_or(NOT_INSTALLED)?;
                if !p.enabled {
                    return Err("Enable this plugin first".into());
                }
                if !p
                    .current
                    .manifest
                    .views
                    .iter()
                    .any(|v| &v.id == view && v.slots.contains(&slot))
                {
                    return Err("This view does not contribute to the selected region".into());
                }
            }
            r.bindings.insert(slot, binding.clone());
            Ok(())
        })
    }

    pub fn binding(&self, slot: Slot) -> Binding {
        let i = self.inner.lock();
        let bound = if i.masked(&Touch::Slot(slot)) {
            None
        } else {
            i.registry.bindings.get(&slot)
        };
        match bound {
            Some(Binding::Plugin { plugin, view })
                if i.package_enabled(plugin)
                    && i.registry.installed.get(plugin).is_some_and(|p| {
                        p.current
                            .manifest
                            .views
                            .iter()
                            .any(|v| &v.id == view && v.slots.contains(&slot))
                    }) =>
            {
                Binding::Plugin {
                    plugin: plugin.clone(),
                    view: view.clone(),
                }
            }
            Some(Binding::Hidden) => Binding::Hidden,
            _ if !i.builtin_enabled(slot.builtin().id) => Binding::Hidden,
            _ => Binding::Builtin,
        }
    }

    /// Whether an open view may still answer requests: still selected, still enabled. Reads no
    /// files, so it is cheap enough for every request.
    pub fn view_selected(&self, slot: Slot, plugin: &str, view: &str) -> Result<(), String> {
        self.selected(slot, plugin, view)?;
        self.active(plugin).map(|_| ())
    }
    fn selected(&self, slot: Slot, plugin: &str, view: &str) -> Result<(), String> {
        let selected = Binding::Plugin {
            plugin: plugin.into(),
            view: view.into(),
        };
        if self.binding(slot) != selected {
            return Err("This view is no longer selected for this region".into());
        }
        Ok(())
    }

    /// A selected view's document, for opening it. Re-hashes the package and checks the document.
    pub fn view(&self, slot: Slot, plugin: &str, view: &str) -> Result<String, String> {
        self.selected(slot, plugin, view)?;
        let (p, dir) = self.activate(plugin)?;
        let view = p
            .manifest
            .views
            .iter()
            .find(|v| v.id == view)
            .ok_or("Unknown view")?;
        open_document(&dir, &view.file)
    }
}

impl Store {
    /// Change a package's preferences, scope, choices or order.
    ///
    /// Not refused while the package has a call in flight, unlike installing: none of these
    /// replaces code, and a runtime is told about the change rather than restarted for it.
    pub fn extension_configure(
        &self,
        plugin: &str,
        change: &serde_json::Value,
    ) -> Result<Snapshot, String> {
        let id = change["id"].as_str().unwrap_or_default();
        let kind = change["kind"].as_str().unwrap_or_default();
        let touches = (kind == "choice")
            .then(|| change["target"].as_str())
            .flatten()
            .map(|target| Touch::Choice(target.into()));
        self.mutate(touches, |r| {
            let manifest = r
                .installed
                .get(plugin)
                .ok_or(NOT_INSTALLED)?
                .current
                .manifest
                .clone();
            let prefs = r.extensions.entry(plugin.into()).or_default();
            match kind {
                "enabled" => {
                    if !manifest
                        .extensions
                        .ids()
                        .iter()
                        .any(|(name, _)| *name == id)
                    {
                        return Err("Unknown contribution".into());
                    }
                    if change["enabled"]
                        .as_bool()
                        .ok_or("Expected enabled state")?
                    {
                        prefs.disabled.remove(id);
                    } else {
                        prefs.disabled.insert(id.into());
                        r.extension_choices
                            .retain(|_, owner| owner != &format!("{plugin}:{id}"));
                    }
                }
                "scope" => {
                    prefs.worktrees =
                        serde_json::from_value(change["worktrees"].clone()).map_err(io)?;
                }
                "setting" => {
                    let setting = manifest
                        .extensions
                        .settings
                        .iter()
                        .find(|s| s.id == id)
                        .ok_or("Unknown setting")?;
                    setting.validate(&change["value"])?;
                    let scope = change["worktree"].as_str().unwrap_or_default();
                    prefs
                        .values
                        .entry(scope.into())
                        .or_default()
                        .insert(id.into(), change["value"].clone());
                }
                "choice" => {
                    let target = change["target"].as_str().ok_or("Target is required")?;
                    if id.is_empty() {
                        r.extension_choices.remove(target);
                    } else if id == BUILTIN_CHOICE {
                        r.extension_choices
                            .insert(target.into(), BUILTIN_CHOICE.into());
                    } else {
                        // A style and an operation replacement can compete for the same name,
                        // so the frontend keeps style choices under `style:<part>` and
                        // replacement choices under the bare operation id.
                        let extensions = &manifest.extensions;
                        let valid = match target.strip_prefix(STYLE_CHOICE) {
                            Some(part) => extensions
                                .styles
                                .iter()
                                .any(|v| v.id == id && v.target == part),
                            None => extensions
                                .actions
                                .iter()
                                .any(|v| v.id == id && v.replaces.as_deref() == Some(target)),
                        };
                        if !valid {
                            return Err("Contribution cannot replace this target".into());
                        }
                        r.extension_choices
                            .insert(target.into(), format!("{plugin}:{id}"));
                    }
                }
                "order" => {
                    let order: Vec<String> =
                        serde_json::from_value(change["order"].clone()).map_err(io)?;
                    if order.len() > 1024
                        || order.iter().collect::<BTreeSet<_>>().len() != order.len()
                    {
                        return Err("Invalid contribution order".into());
                    }
                    r.extension_order = order;
                }
                _ => return Err("Unknown contribution preference".into()),
            }
            Ok(())
        })
    }
    /// Declared access and the caller's workspace. Resource arguments are the host's to resolve.
    pub fn extension_allowed(
        &self,
        plugin: &str,
        operation: &str,
        worktree: &str,
    ) -> Result<(), String> {
        let (package, _) = self.active(plugin)?;
        if !package
            .manifest
            .extensions
            .access
            .iter()
            .any(|v| v == operation)
        {
            return Err("This operation was not declared in the reviewed host access".into());
        }
        if self
            .extension_scope(plugin)
            .is_some_and(|scope| !scope.contains(worktree))
        {
            return Err("Plugin is not active for this worktree".into());
        }
        Ok(())
    }
    /// The worktrees a package is limited to, or `None` when it may act in every workspace.
    pub fn extension_scope(&self, plugin: &str) -> Option<BTreeSet<String>> {
        self.inner
            .lock()
            .registry
            .extensions
            .get(plugin)
            .map(|p| p.worktrees.clone())
            .filter(|w| !w.is_empty())
    }
    pub fn runtime_invocation(&self, plugin: &str) -> Result<Invocation, String> {
        let (package, dir) = self.activate(plugin)?;
        if package.manifest.extensions.runtime.is_empty() {
            return Err("Plugin has no activation runtime".into());
        }
        let argv = package.manifest.extensions.runtime.clone();
        self.launch(&package, &dir, &argv, &dir, 2, RUNTIME_DEADLINE_MS)
    }
}
