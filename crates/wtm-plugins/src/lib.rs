//! Reviewed package snapshots, not live code loaded into the privileged webview.
//!
//! A development link records its source but executes a reviewed copy. Otherwise an editor save
//! between approval and spawn could change what was approved. Native commands remain trusted code,
//! not an OS sandbox; the digest is an integrity check, not a claim about an author's intentions.
pub mod contributions;
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
use std::path::{Path, PathBuf};
use std::sync::Arc;
use wtm_core::ports::exec::{CancelToken, CommandRunner, Invocation};

const MAX_BYTES: u64 = 16 * 1024 * 1024;
const MAX_FILES: usize = 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Package {
    pub manifest: Manifest,
    pub source: String,
    pub kind: String,
    pub revision: String,
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
            experience: "wtm.chat".into(),
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
    pub replacing: Option<String>,
}
#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub registry: Registry,
    pub problem: Option<String>,
    pub builtins: &'static [contributions::Builtin],
}
struct Inner {
    registry: Registry,
    problem: Option<String>,
    pending: BTreeMap<String, Review>,
    running: BTreeMap<String, usize>,
    attached: BTreeSet<(String, String, String)>,
    view_requests: BTreeMap<String, usize>,
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
impl Store {
    pub fn new(root: PathBuf, runner: Arc<dyn CommandRunner>, safe_mode: bool) -> Self {
        let loaded = std::fs::read(root.join("registry.json"));
        let (mut registry, problem) = match loaded {
            Ok(bytes) => match serde_json::from_slice::<Registry>(&bytes) {
                Ok(r) if r.version == 1 => (r, None),
                _ => (Registry::default(), Some("The plugin registry is unreadable. Recovery preserves it before resetting plugin settings.".into())),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Registry::default(), None),
            Err(e) => (Registry::default(), Some(e.to_string())),
        };
        if safe_mode || problem.is_some() {
            registry.safe_mode = true;
            registry.bindings.clear();
            registry.disabled.clear();
            registry.experience = "wtm.chat".into();
            for p in registry.installed.values_mut() {
                p.enabled = false;
            }
        }
        Self {
            root,
            runner,
            inner: Mutex::new(Inner {
                registry,
                problem,
                pending: BTreeMap::new(),
                running: BTreeMap::new(),
                attached: BTreeSet::new(),
                view_requests: BTreeMap::new(),
            }),
        }
    }
    pub fn snapshot(&self) -> Snapshot {
        let i = self.inner.lock();
        Snapshot {
            registry: i.registry.clone(),
            problem: i.problem.clone(),
            builtins: BUILTINS,
        }
    }
    pub fn enabled(&self, id: &str) -> bool {
        let i = self.inner.lock();
        if BUILTINS.iter().any(|b| b.id == id) {
            !i.registry.disabled.contains(id)
        } else {
            !i.registry.safe_mode && i.registry.installed.get(id).is_some_and(|p| p.enabled)
        }
    }
    fn persist(&self, registry: &Registry) -> Result<(), String> {
        std::fs::create_dir_all(&self.root).map_err(io)?;
        let temp = self
            .root
            .join(format!("registry-{}.tmp", uuid::Uuid::new_v4()));
        std::fs::write(&temp, serde_json::to_vec_pretty(registry).map_err(io)?).map_err(io)?;
        std::fs::rename(&temp, self.root.join("registry.json")).map_err(io)
    }
    fn mutate(
        &self,
        id: &str,
        f: impl FnOnce(&mut Registry) -> Result<(), String>,
    ) -> Result<Snapshot, String> {
        let mut i = self.inner.lock();
        if let Some(problem) = &i.problem {
            return Err(problem.clone());
        }
        ensure_idle(&i, id)?;
        let mut next = i.registry.clone();
        f(&mut next)?;
        self.persist(&next)?;
        i.registry = next;
        Ok(Snapshot {
            registry: i.registry.clone(),
            problem: None,
            builtins: BUILTINS,
        })
    }
    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<Snapshot, String> {
        self.mutate(id, |r| {
            if !enabled && id == "wtm.chat" && !r.disabled.contains("wtm.home") {
                return Err("Disable Home before disabling its WTM Chat dependency".into());
            }
            if enabled && id == "wtm.home" && r.disabled.contains("wtm.chat") {
                return Err("Enable WTM Chat before enabling Home".into());
            }
            if !enabled && r.experience == id {
                return Err("Choose another default experience first".into());
            }
            if BUILTINS.iter().any(|b| b.id == id) {
                if enabled {
                    r.disabled.remove(id);
                } else {
                    r.disabled.insert(id.into());
                }
            } else {
                r.installed
                    .get_mut(id)
                    .ok_or("Plugin is not installed")?
                    .enabled = enabled;
                if enabled {
                    r.safe_mode = false;
                } else {
                    r.extension_choices
                        .retain(|_, owner| !owner.starts_with(&format!("{id}:")));
                    r.bindings.retain(
                        |_, b| !matches!(b, Binding::Plugin { plugin, .. } if plugin == id),
                    );
                }
            }
            Ok(())
        })
    }
    pub fn experience(&self, id: &str) -> Result<Snapshot, String> {
        self.mutate("", |r| {
            if !["wtm.chat", "wtm.native-cli"].contains(&id) || r.disabled.contains(id) {
                return Err("Choose an enabled agent experience".into());
            }
            r.experience = id.into();
            Ok(())
        })
    }
    pub fn recover(&self) -> Result<Snapshot, String> {
        let mut i = self.inner.lock();
        if i.running.values().any(|n| *n > 0) {
            return Err("Wait for running plugin commands before recovery".into());
        }
        if i.problem.is_some() && self.root.join("registry.json").exists() {
            std::fs::rename(
                self.root.join("registry.json"),
                self.root
                    .join(format!("registry-corrupt-{}.json", uuid::Uuid::new_v4())),
            )
            .map_err(io)?;
        }
        let mut next = i.registry.clone();
        i.attached.clear();
        next.safe_mode = true;
        next.bindings.clear();
        next.extension_choices.clear();
        next.disabled.clear();
        next.experience = "wtm.chat".into();
        for p in next.installed.values_mut() {
            p.enabled = false;
        }
        self.persist(&next)?;
        i.registry = next;
        i.problem = None;
        Ok(Snapshot {
            registry: i.registry.clone(),
            problem: None,
            builtins: BUILTINS,
        })
    }
    pub fn prepare(&self, source: &str, kind: &str, revision: &str) -> Result<Review, String> {
        if !["local", "git"].contains(&kind) {
            return Err("Choose a local folder or Git repository".into());
        }
        let token = uuid::Uuid::new_v4().to_string();
        let directory = self.root.join("packages").join(&token);
        std::fs::create_dir_all(&directory).map_err(io)?;
        let result = (|| {
            let (source, resolved) = if kind == "git" {
                if !source.starts_with("https://")
                    || source.contains('@')
                    || source.contains(['\n', '\r', '?', '#'])
                {
                    return Err(
                        "Use an HTTPS Git URL without embedded credentials, query or fragment"
                            .into(),
                    );
                }
                self.git(
                    &self.root,
                    &[
                        "clone",
                        "--no-checkout",
                        "--template=",
                        "--",
                        source,
                        directory.to_str().ok_or("Invalid package path")?,
                    ],
                )?;
                if revision.is_empty() {
                    return Err("Choose an explicit tag, branch or commit".into());
                }
                let commit = self
                    .git(
                        &directory,
                        &[
                            "rev-parse",
                            "--verify",
                            "--end-of-options",
                            &format!("{revision}^{{commit}}"),
                        ],
                    )?
                    .trim()
                    .to_owned();
                if !commit.bytes().all(|b| b.is_ascii_hexdigit()) || commit.len() != 40 {
                    return Err("Git returned an invalid commit".into());
                }
                self.git(&directory, &["checkout", "--detach", "--force", &commit])?;
                std::fs::remove_dir_all(directory.join(".git")).map_err(io)?;
                (source.to_owned(), commit)
            } else {
                if !Path::new(source).is_absolute() {
                    return Err("Use an absolute local folder path".into());
                }
                let path = Path::new(source).canonicalize().map_err(io)?;
                if self.root.starts_with(&path) || path.starts_with(&self.root) {
                    return Err("A plugin source cannot contain WTM's plugin storage".into());
                }
                let files = files(&path)?;
                for (name, _) in files {
                    let target = directory.join(&name);
                    if let Some(parent) = target.parent() {
                        std::fs::create_dir_all(parent).map_err(io)?;
                    }
                    std::fs::copy(path.join(name), target).map_err(io)?;
                }
                (
                    path.to_string_lossy().into_owned(),
                    "Development snapshot".into(),
                )
            };
            let digest = fingerprint(&directory)?;
            let text = std::fs::read_to_string(directory.join("wtm-plugin.toml")).map_err(io)?;
            let manifest = Manifest::parse(&text)?;
            for file in manifest
                .panels
                .iter()
                .map(|p| &p.file)
                .chain(manifest.views.iter().map(|v| &v.file))
            {
                if !directory.join(file).is_file() {
                    return Err(format!("Missing view file: {file}"));
                }
            }
            let mut inner = self.inner.lock();
            if inner.pending.len() >= 16 {
                return Err("Too many pending reviews; cancel an earlier review first".into());
            }
            let review = Review {
                token: token.clone(),
                replacing: inner
                    .registry
                    .installed
                    .get(&manifest.id)
                    .map(|p| p.current.digest.clone()),
                package: Package {
                    manifest,
                    source,
                    kind: kind.into(),
                    revision: resolved,
                    digest,
                    directory: token,
                },
            };
            inner.pending.insert(review.token.clone(), review.clone());
            Ok(review)
        })();
        if result.is_err() {
            let _ = std::fs::remove_dir_all(&directory);
        }
        result
    }
    fn git(&self, cwd: &Path, arguments: &[&str]) -> Result<String, String> {
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
        let mut inv = Invocation::new(argv, cwd, 120_000);
        inv.env = BTreeMap::from([
            ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
            ("GIT_CONFIG_SYSTEM".into(), "/dev/null".into()),
            ("GIT_CONFIG_COUNT".into(), "0".into()),
            ("GIT_TERMINAL_PROMPT".into(), "0".into()),
        ]);
        self.runner
            .run(&inv, &CancelToken::new())
            .map(|o| o.stdout)
            .map_err(io)
    }
    pub fn cancel(&self, token: &str) -> Result<(), String> {
        if let Some(review) = self.inner.lock().pending.remove(token) {
            std::fs::remove_dir_all(self.directory(&review.package)?).map_err(io)?;
        }
        Ok(())
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
        self.verify(&review.package)?;
        let id = &review.package.manifest.id;
        ensure_idle(&i, id)?;
        let previous = i.registry.installed.get(id).map(|p| p.current.clone());
        if previous.as_ref().map(|p| &p.digest) != review.replacing.as_ref() {
            return Err("The installed version changed during review; review again".into());
        }
        let mut next = i.registry.clone();
        next.installed.insert(
            id.clone(),
            Installed {
                current: review.package,
                previous,
                enabled: true,
            },
        );
        next.safe_mode = false;
        self.persist(&next)?;
        i.registry = next;
        i.pending.remove(token);
        Ok(Snapshot {
            registry: i.registry.clone(),
            problem: None,
            builtins: BUILTINS,
        })
    }
    pub fn uninstall(&self, id: &str) -> Result<Snapshot, String> {
        self.mutate(id, |r| {
            r.installed.remove(id).ok_or("Plugin is not installed")?;
            r.extension_choices
                .retain(|_, owner| !owner.starts_with(&format!("{id}:")));
            r.bindings
                .retain(|_, b| !matches!(b, Binding::Plugin { plugin, .. } if plugin == id));
            Ok(())
        })
    }
    pub fn rollback(&self, id: &str) -> Result<Snapshot, String> {
        self.mutate(id, |r| {
            let p = r.installed.get_mut(id).ok_or("Plugin is not installed")?;
            let previous = p.previous.take().ok_or("No previous version is retained")?;
            self.verify(&previous)?;
            p.previous = Some(std::mem::replace(&mut p.current, previous));
            Ok(())
        })
    }
    fn directory(&self, p: &Package) -> Result<PathBuf, String> {
        uuid::Uuid::parse_str(&p.directory).map_err(io)?;
        Ok(self.root.join("packages").join(&p.directory))
    }
    fn verify(&self, p: &Package) -> Result<PathBuf, String> {
        let dir = self.directory(p)?;
        if fingerprint(&dir)? != p.digest {
            return Err(
                "Package content changed since review. Reload and approve it again.".into(),
            );
        }
        let actual =
            Manifest::parse(&std::fs::read_to_string(dir.join("wtm-plugin.toml")).map_err(io)?)?;
        if actual != p.manifest {
            return Err("Stored manifest differs from reviewed package".into());
        }
        Ok(dir)
    }
    pub fn active(&self, id: &str) -> Result<(Package, PathBuf), String> {
        let p = {
            let i = self.inner.lock();
            if i.registry.safe_mode {
                return Err("User plugins are disabled in recovery mode".into());
            }
            let p = i
                .registry
                .installed
                .get(id)
                .ok_or("Plugin is not installed")?;
            if !p.enabled {
                return Err("Plugin is disabled".into());
            }
            p.current.clone()
        };
        let dir = self.verify(&p)?;
        Ok((p, dir))
    }
    pub fn panel(&self, id: &str, contribution: &str) -> Result<String, String> {
        let (p, dir) = self.active(id)?;
        let panel = p
            .manifest
            .panels
            .iter()
            .find(|p| p.id == contribution)
            .ok_or("Unknown panel")?;
        manifest::relative(&panel.file)?;
        std::fs::read_to_string(dir.join(&panel.file)).map_err(io)
    }
    pub fn terminal(&self, id: &str, contribution: &str) -> Result<Vec<String>, String> {
        let (package, dir) = self.active(id)?;
        let entry = package
            .manifest
            .terminals
            .iter()
            .find(|c| c.id == contribution)
            .ok_or("Unknown terminal profile")?;
        let mut argv = entry.command.clone();
        if argv[0].starts_with("./") {
            argv[0] = dir.join(&argv[0]).to_string_lossy().into_owned();
        }
        Ok(argv)
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
        let mut argv = command.command.clone();
        if argv[0].starts_with('.') {
            manifest::relative(&argv[0])?;
            argv[0] = dir.join(&argv[0]).to_string_lossy().into_owned();
        }
        let mut inv = Invocation::new(argv, cwd, 30_000).with_stdin(request);
        let data = self.root.join("data").join(&p.manifest.id);
        std::fs::create_dir_all(&data).map_err(io)?;
        inv.env = BTreeMap::from([
            ("WTM_PLUGIN_ROOT".into(), dir.to_string_lossy().into_owned()),
            (
                "WTM_PLUGIN_DATA".into(),
                data.to_string_lossy().into_owned(),
            ),
            ("WTM_PLUGIN_API".into(), "1".into()),
        ]);
        let result = self.runner.run(&inv, &CancelToken::new()).map_err(io)?;
        if result.stdout.len() > 1024 * 1024 {
            return Err("Plugin response exceeds one MiB".into());
        }
        Ok(result.stdout)
    }
}
fn files(root: &Path) -> Result<Vec<(PathBuf, u64)>, String> {
    if !root.symlink_metadata().map_err(io)?.is_dir() {
        return Err("Package root must be a real directory".into());
    }
    let mut entries = 0;
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    let mut total = 0;
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(dir).map_err(io)? {
            let entry = entry.map_err(io)?;
            entries += 1;
            if entries > MAX_FILES {
                return Err("Package exceeds 1024 entries".into());
            }
            let name = entry.file_name();
            if name == ".git" {
                continue;
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
                return Err("Plugin packages may contain only regular files and directories, not symlinks or devices".into());
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
    for (path, _) in files(root)? {
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

fn ensure_idle(inner: &Inner, id: &str) -> Result<(), String> {
    if inner.running.get(id).is_some_and(|n| *n > 0)
        || inner.attached.iter().any(|(p, _, _)| p == id)
    {
        return Err("Detach this plugin's sessions and wait for its commands before changing its version or activation".into());
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
    /// Hold across asynchronous host activation so replacement cannot race an opening view.
    pub fn activation(&self, id: &str) -> Activation<'_> {
        *self.inner.lock().running.entry(id.into()).or_default() += 1;
        Activation {
            store: self,
            plugin: id.into(),
        }
    }

    pub fn bind(&self, slot: Slot, binding: Binding) -> Result<Snapshot, String> {
        self.mutate("", |r| {
            if binding == Binding::Builtin {
                if slot == Slot::Home && r.disabled.contains("wtm.chat") {
                    return Err("Enable WTM Chat before enabling Home".into());
                }
                r.disabled.remove(slot.builtin().id);
            }
            if let Binding::Plugin { plugin, view } = &binding {
                let p = r.installed.get(plugin).ok_or("Plugin is not installed")?;
                if r.safe_mode || !p.enabled {
                    return Err("Enable this plugin first".into());
                }
                self.verify(&p.current)?;
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
            r.bindings.insert(slot, binding);
            Ok(())
        })
    }

    pub fn binding(&self, slot: Slot) -> Binding {
        let i = self.inner.lock();
        if i.registry.safe_mode {
            return Binding::Builtin;
        }
        match i.registry.bindings.get(&slot) {
            Some(Binding::Plugin { plugin, view })
                if i.registry.installed.get(plugin).is_some_and(|p| {
                    p.enabled
                        && p.current
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
            _ if i.registry.disabled.contains(slot.builtin().id) => Binding::Hidden,
            _ => Binding::Builtin,
        }
    }

    pub fn view(&self, slot: Slot, plugin: &str, view: &str) -> Result<String, String> {
        if self.binding(slot)
            != (Binding::Plugin {
                plugin: plugin.into(),
                view: view.into(),
            })
        {
            return Err("This view is no longer selected for this region".into());
        }
        let (p, dir) = self.active(plugin)?;
        let view = p
            .manifest
            .views
            .iter()
            .find(|v| v.id == view)
            .ok_or("Unknown view")?;
        std::fs::read_to_string(dir.join(&view.file)).map_err(io)
    }
}

impl Store {
    pub fn extension_configure(
        &self,
        plugin: &str,
        change: &serde_json::Value,
    ) -> Result<Snapshot, String> {
        self.mutate(plugin, |r| {
            let manifest = r
                .installed
                .get(plugin)
                .ok_or("Plugin is not installed")?
                .current
                .manifest
                .clone();
            let id = change["id"].as_str().unwrap_or_default();
            let prefs = r.extensions.entry(plugin.into()).or_default();
            match change["kind"].as_str().unwrap_or_default() {
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
                    } else if id == "builtin" {
                        r.extension_choices.insert(target.into(), "builtin".into());
                    } else {
                        let valid = manifest
                            .extensions
                            .styles
                            .iter()
                            .any(|v| v.id == id && v.target == target)
                            || manifest.extensions.content.iter().any(|v| {
                                v.id == id
                                    && v.target == target
                                    && v.position == extensions::Position::Replace
                            })
                            || manifest
                                .extensions
                                .actions
                                .iter()
                                .any(|v| v.id == id && v.replaces.as_deref() == Some(target));
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
        let i = self.inner.lock();
        if i.registry
            .extensions
            .get(plugin)
            .is_some_and(|p| !p.worktrees.is_empty() && !p.worktrees.contains(worktree))
        {
            return Err("Plugin is not active for this workspace".into());
        }
        Ok(())
    }
    pub fn runtime_invocation(&self, plugin: &str) -> Result<Invocation, String> {
        let (package, dir) = self.active(plugin)?;
        let mut argv = package.manifest.extensions.runtime;
        if argv.is_empty() {
            return Err("Plugin has no activation runtime".into());
        }
        if argv[0].starts_with("./") {
            argv[0] = dir.join(&argv[0]).to_string_lossy().into_owned();
        }
        let data = self.root.join("data").join(plugin);
        std::fs::create_dir_all(&data).map_err(io)?;
        Ok(
            Invocation::new(argv, &dir, 604_800_000).with_env(BTreeMap::from([
                ("WTM_PLUGIN_ROOT".into(), dir.to_string_lossy().into_owned()),
                (
                    "WTM_PLUGIN_DATA".into(),
                    data.to_string_lossy().into_owned(),
                ),
                ("WTM_PLUGIN_API".into(), "2".into()),
            ])),
        )
    }
}
