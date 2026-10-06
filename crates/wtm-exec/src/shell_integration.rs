//! ZLE is the final admission gate: only it can inspect the actual edit buffer.
//!
//! A separate socket wakes a widget while ZLE owns input. Sending a key sequence
//! through the PTY would race a user starting a foreground job, which could then
//! receive the trigger bytes. Unsupported shells retain their ordinary behavior.

use std::fs::{DirBuilder, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

pub struct ShellIntegration {
    directory: PathBuf,
    token: String,
}

impl std::fmt::Debug for ShellIntegration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellIntegration").finish_non_exhaustive()
    }
}

fn quote(path: &Path) -> String {
    shell_words::quote(&path.to_string_lossy()).into_owned()
}

impl ShellIntegration {
    pub fn create(socket: &Path, helper: &Path, original: &Path) -> Result<Self, String> {
        let integration = Self {
            directory: std::env::temp_dir().join(format!("wtm-zle-{}", uuid::Uuid::new_v4())),
            token: uuid::Uuid::new_v4().simple().to_string(),
        };
        DirBuilder::new()
            .mode(0o700)
            .create(&integration.directory)
            .map_err(|e| e.to_string())?;
        let wrapper = quote(&integration.directory);
        // Restore the user's ZDOTDIR around every startup file. A .zshenv may
        // change it, and sourcing only ~/.zshrc would silently skip that chain.
        integration.write(
            ".zshenv",
            &format!(
                "typeset -g _wtm_dotdir={}\nZDOTDIR=$_wtm_dotdir\n[[ ! -r $ZDOTDIR/.zshenv ]] || builtin source $ZDOTDIR/.zshenv\n_wtm_dotdir=${{ZDOTDIR:-$HOME}}\nZDOTDIR={wrapper}\n",
                quote(original)
            ),
        )?;
        integration.write(
            ".zprofile",
            &format!(
                "ZDOTDIR=$_wtm_dotdir\n[[ ! -r $ZDOTDIR/.zprofile ]] || builtin source $ZDOTDIR/.zprofile\n_wtm_dotdir=${{ZDOTDIR:-$HOME}}\nZDOTDIR={wrapper}\n"
            ),
        )?;
        integration.write(
            ".zshrc",
            &format!(
                "ZDOTDIR=$_wtm_dotdir\n[[ ! -r $ZDOTDIR/.zshrc ]] || builtin source $ZDOTDIR/.zshrc\nbuiltin source {wrapper}/integration.zsh\n"
            ),
        )?;
        let hook = include_str!("shell_integration.zsh")
            .replace("@SOCKET@", &quote(socket))
            .replace("@HELPER@", &quote(helper))
            .replace("@TOKEN@", &integration.token);
        integration.write("integration.zsh", &hook)?;
        Ok(integration)
    }

    fn write(&self, name: &str, contents: &str) -> Result<(), String> {
        OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(self.directory.join(name))
            .and_then(|mut file| file.write_all(contents.as_bytes()))
            .map_err(|e| e.to_string())
    }

    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    #[must_use]
    pub fn token(&self) -> &str {
        &self.token
    }
}

impl Drop for ShellIntegration {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::sync::Arc;
    use std::time::Duration;
    use wtm_core::model::{ExitOutcome, SessionId};
    use wtm_core::ports::clock::Clock;
    use wtm_core::ports::exec::Invocation;
    use wtm_core::ports::pty::{PtyHost, PtySink};

    #[derive(Default)]
    struct Output(parking_lot::Mutex<Vec<u8>>, parking_lot::Condvar);

    impl PtySink for Output {
        fn on_output(&self, _: &SessionId, bytes: &[u8]) {
            self.0.lock().extend_from_slice(bytes);
            self.1.notify_all();
        }

        fn on_exit(&self, _: &SessionId, _: &ExitOutcome) {}
    }

    impl Output {
        fn contains(&self, text: &str) -> bool {
            String::from_utf8_lossy(&self.0.lock()).contains(text)
        }

        fn await_text(&self, text: &str) {
            let clock = crate::clock::SystemClock::new();
            let deadline = clock.monotonic_ms() + 5_000;
            let mut bytes = self.0.lock();
            while !String::from_utf8_lossy(&bytes).contains(text) {
                assert!(
                    clock.monotonic_ms() < deadline,
                    "{:?}",
                    String::from_utf8_lossy(&bytes)
                );
                self.1.wait_for(&mut bytes, Duration::from_millis(20));
            }
        }
    }

    struct Terminal {
        host: crate::PtyHostImpl,
        session: SessionId,
        reader: BufReader<UnixStream>,
        output: Arc<Output>,
        _root: tempfile::TempDir,
        integration: ShellIntegration,
    }

    impl Terminal {
        fn open() -> Self {
            // Keep the socket short enough for macOS sun_path, independent of
            // the test runner's long per-process TMPDIR.
            let root = tempfile::tempdir_in("/tmp").unwrap();
            let socket = root.path().join("s");
            let listener = UnixListener::bind(&socket).unwrap();
            listener.set_nonblocking(true).unwrap();
            let profiles = root.path().join("profiles");
            std::fs::create_dir(&profiles).unwrap();
            std::fs::write(profiles.join(".zshenv"), "export WTM_CHAIN=env\n").unwrap();
            std::fs::write(profiles.join(".zprofile"), "WTM_CHAIN+=:profile\n").unwrap();
            std::fs::write(
                profiles.join(".zshrc"),
                "WTM_CHAIN+=:rc\nPROMPT='READY> '\n",
            )
            .unwrap();
            std::fs::write(
                profiles.join(".zlogin"),
                "WTM_CHAIN+=:login\nprint -r -- $WTM_CHAIN\n",
            )
            .unwrap();
            let helper = root.path().join("test helper");
            std::fs::write(&helper, "#!/bin/sh\nprintf 'HELPER:%s\\n' \"$3\"\n").unwrap();
            std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
            let integration = ShellIntegration::create(&socket, &helper, &profiles).unwrap();
            let mut invocation =
                Invocation::new(vec!["/bin/zsh".into(), "-il".into()], root.path(), 15_000);
            invocation
                .env
                .insert("HOME".into(), root.path().to_string_lossy().into_owned());
            invocation.env.insert(
                "ZDOTDIR".into(),
                integration.directory().to_string_lossy().into_owned(),
            );
            // A user's Apple Terminal session restore is unrelated to this PTY.
            invocation
                .env
                .insert("SHELL_SESSIONS_DISABLE".into(), "1".into());
            let host = crate::PtyHostImpl::new(crate::ResolvedPath::resolve(Some("/usr/bin:/bin")));
            let output = Arc::new(Output::default());
            let session = host
                .spawn(&invocation, 24, 100, None, output.clone())
                .unwrap()
                .session;
            let clock = crate::clock::SystemClock::new();
            let deadline = clock.monotonic_ms() + 5_000;
            let stream = loop {
                if let Ok((stream, _)) = listener.accept() {
                    break stream;
                }
                assert!(
                    clock.monotonic_ms() < deadline,
                    "shell failed to connect: {:?}",
                    String::from_utf8_lossy(&output.0.lock())
                );
                std::thread::sleep(Duration::from_millis(10));
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut terminal = Self {
                host,
                session,
                reader: BufReader::new(stream),
                output,
                _root: root,
                integration,
            };
            assert!(terminal.line().contains(terminal.integration.token()));
            terminal.output.await_text("env:profile:rc:login");
            terminal
        }

        fn line(&mut self) -> String {
            let mut line = String::new();
            self.reader.read_line(&mut line).unwrap_or_else(|error| {
                panic!(
                    "{error}: {:?}",
                    String::from_utf8_lossy(&self.output.0.lock())
                )
            });
            assert!(!line.is_empty());
            line.trim().into()
        }

        fn prompt(&mut self) -> u64 {
            loop {
                let line = self.line();
                if line.starts_with("prompt ") {
                    return line.split_whitespace().nth(1).unwrap().parse().unwrap();
                }
            }
        }

        fn input(&self, bytes: &[u8]) {
            self.host.write(&self.session, bytes).unwrap();
        }

        fn dispatch(&mut self, epoch: u64, capability: &str) {
            // Match production: publish the complete line, not formatting fragments which can
            // wake ZLE before the sending thread has finished writing the request.
            self.reader
                .get_mut()
                .write_all(format!("run {epoch} {capability}\n").as_bytes())
                .unwrap();
        }
    }

    impl Drop for Terminal {
        fn drop(&mut self) {
            let _ = self.host.kill(&self.session);
        }
    }

    #[test]
    fn socket_admission_preserves_profiles_and_refuses_edited_continuation_and_background_prompts()
    {
        let mut terminal = Terminal::open();
        let epoch = terminal.prompt();
        assert!(terminal.host.shell_owns_foreground(&terminal.session));
        let accepted = "11111111111111111111111111111111";
        terminal.dispatch(epoch, accepted);
        terminal.output.await_text(&format!("HELPER:{accepted}"));
        let epoch = terminal.prompt();

        terminal.input(b"untouched");
        terminal.output.await_text("untouched");
        let refused = "22222222222222222222222222222222";
        terminal.dispatch(epoch, refused);
        assert_eq!(terminal.line(), format!("refused {refused}"));
        terminal.input(b"\x03");
        terminal.prompt();
        terminal.input(b"echo '\n");
        let epoch = terminal.prompt();
        terminal.dispatch(epoch, refused);
        assert_eq!(terminal.line(), format!("refused {refused}"));
        terminal.input(b"\x03");
        terminal.prompt();
        terminal.input(b"sleep 10 &\n");
        let epoch = terminal.prompt();
        terminal.dispatch(epoch, refused);
        assert_eq!(terminal.line(), format!("refused {refused}"));
        assert!(!terminal.output.contains(&format!("HELPER:{refused}")));
        terminal.input(b"kill %1\n");
    }

    #[test]
    fn a_control_message_queued_during_a_job_cannot_run_at_its_later_prompt() {
        let mut terminal = Terminal::open();
        let epoch = terminal.prompt();
        terminal.input(b"/bin/sh -c 'printf JOB-STARTED\\\\n; sleep 0.3'\n");
        assert_eq!(terminal.line(), format!("busy {epoch}"));
        terminal.output.await_text("JOB-STARTED\r\n");
        assert!(!terminal.host.shell_owns_foreground(&terminal.session));
        let capability = "33333333333333333333333333333333";
        terminal.dispatch(epoch, capability);
        assert!(terminal.prompt() > epoch);
        assert_eq!(terminal.line(), format!("refused {capability}"));
        assert!(!terminal.output.contains(&format!("HELPER:{capability}")));
    }
}
