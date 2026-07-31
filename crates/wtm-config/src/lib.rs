//! Configuration: layering, validation, and trust.
//!
//! - [`paths`] — where wtm keeps its own files (`~/.config/wtm`, deliberately not
//!   `~/Library/Application Support`).
//! - [`user`] — the app config: registered repositories, UI preferences, exec settings.
//! - [`layers`] — the four-layer precedence chain and the per-key TOML merge.
//! - [`trust`] — approval bound to a **content hash**, so editing a config re-arms the
//!   prompt. A `wtm.toml` declares shell commands and lives inside a repository, which
//!   makes opening an unfamiliar repo equivalent to running its code without this.
//! - [`validate`] — the semantic checks that turn a type-correct but wrong config into
//!   a load-time error naming a file and a key.
//! - [`fs`] — the real, read-only `FileStore`.
//! - [`store`] — [`FileConfigStore`], the `ConfigStore` port.
//!
//! # This crate depends on the domain alone
//!
//! Validation needs to check templates, but it takes `&dyn TemplateEngine` rather than
//! importing `wtm-render`. The real engine appears only as a dev-dependency, for
//! testing the validator.

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod fs;
pub mod layers;
pub mod paths;
pub mod store;
pub mod trust;
pub mod user;
pub mod validate;

pub use fs::{RealFileStore, absolutize, parse_dotenv};
pub use layers::{BUILT_IN_DEFAULTS, LOCAL_FILENAME, LayerPaths, REPO_FILENAME};
pub use paths::AppPaths;
pub use store::FileConfigStore;
pub use trust::{TrustStore, content_hash};
pub use user::{PaletteDef, Theme, UserConfig};
pub use validate::check_forbidden;
