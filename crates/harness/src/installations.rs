//! Device-local, explicit runtime selection. Updates never replace a running executable.
//! Credentials and user configuration are deliberately outside installation directories.
use crate::{
    HarnessError,
    adapter_install::{self, NpmPin},
};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use zeron_proto::HarnessId;

pub const HARNESSES: [HarnessId; 8] = [
    HarnessId::ClaudeCode,
    HarnessId::Codex,
    HarnessId::Cursor,
    HarnessId::Devin,
    HarnessId::Grok,
    HarnessId::Hermes,
    HarnessId::Pi,
    HarnessId::Opencode,
];

pub fn name(h: HarnessId) -> &'static str {
    match h {
        HarnessId::ClaudeCode => "claude",
        HarnessId::Codex => "codex",
        HarnessId::Cursor => "cursor-agent",
        HarnessId::Devin => "devin",
        HarnessId::Grok => "grok",
        HarnessId::Hermes => "hermes",
        HarnessId::Pi => "pi",
        HarnessId::Opencode => "opencode",
        HarnessId::Mock => "mock",
    }
}
fn package(h: HarnessId) -> Option<(&'static str, &'static str)> {
    Some(match h {
        HarnessId::Codex => ("@openai/codex@0.153.3", "codex"),
        HarnessId::ClaudeCode => ("@anthropic-ai/claude-code@2.1.258", "claude"),
        HarnessId::Grok => ("@xai-official/grok@1.0.4", "grok"),
        HarnessId::Opencode => ("opencode-ai@1.18.21", "opencode"),
        HarnessId::Pi => ("@earendil-works/pi-coding-agent@0.85.1", "pi"),
        _ => return None,
    })
}
pub fn root() -> Option<PathBuf> {
    std::env::var_os("ZERON_HARNESSES_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|v| PathBuf::from(v).join(".zeron/harnesses")))
}
fn recommended(h: HarnessId) -> Option<&'static str> {
    if h == HarnessId::Cursor {
        return Some(NpmPin::parse(crate::cursor::CURSOR_SDK_PIN).version);
    }
    if h == HarnessId::Devin {
        Some(crate::installations_download::DEVIN_VERSION)
    } else {
        package(h).map(|(p, _)| NpmPin::parse(p).version)
    }
}
fn directory(h: HarnessId) -> Option<PathBuf> {
    root().map(|p| p.join(name(h)))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeChoice {
    pub executable: PathBuf,
    pub version: String,
    pub managed: bool,
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Selection {
    current: Option<RuntimeChoice>,
    previous: Option<RuntimeChoice>,
}

fn read(dir: &Path) -> Result<Selection, HarnessError> {
    match std::fs::read(dir.join("selection.json")) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| HarnessError::Install(
            "Runtime selection is unreadable. Select an installation again in Settings → Harnesses.".into())),
        Err(e) if e.kind()==std::io::ErrorKind::NotFound => Ok(Selection::default()),
        Err(e) => Err(e.into()),
    }
}
/// A broken explicit selection must never silently fall back to an unrelated PATH binary.
pub fn selected(h: HarnessId) -> Option<PathBuf> {
    let dir = directory(h)?;
    match read(&dir) {
        Ok(s) => s.current.map(|c| c.executable),
        Err(_) => Some(dir.join("invalid-selection")),
    }
}
pub fn discovered(h: HarnessId) -> Option<PathBuf> {
    if h == HarnessId::Cursor {
        return None;
    }

    let variable = match h {
        HarnessId::ClaudeCode => "CLAUDE_EXECUTABLE",
        HarnessId::Codex => "CODEX_EXECUTABLE",
        HarnessId::Cursor => "CURSOR_EXECUTABLE",
        HarnessId::Devin => "DEVIN_EXECUTABLE",
        HarnessId::Grok => "GROK_EXECUTABLE",
        HarnessId::Hermes => "HERMES_EXECUTABLE",
        HarnessId::Pi => "PI_EXECUTABLE",
        HarnessId::Opencode => "OPENCODE_EXECUTABLE",
        HarnessId::Mock => return None,
    };
    if let Some(path) = std::env::var_os(variable).filter(|v| !v.is_empty()) {
        return Some(path.into());
    }
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let exe = if cfg!(windows) {
        format!("{}.exe", name(h))
    } else {
        name(h).into()
    };
    let mut extra = vec![
        home.join(".local/bin").join(&exe),
        home.join(".npm-global/bin").join(&exe),
        home.join(format!(".{}/bin", name(h))).join(&exe),
    ];
    if h == HarnessId::Devin {
        extra.push(
            home.join(".local/share/devin/cli/_versions/current/bin")
                .join(&exe),
        );
    }
    if h == HarnessId::Hermes {
        extra.push(home.join(".hermes/hermes-agent/venv/bin/hermes"));
    }
    crate::acp::find_on_paths(&exe, extra)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Installation {
    pub harness: HarnessId,
    pub installed: bool,
    pub version: Option<String>,
    pub managed: bool,
    pub executable: Option<PathBuf>,
    pub recommended_version: Option<String>,
    pub previous_version: Option<String>,
    pub can_install: bool,
    pub error: Option<String>,
    #[serde(default)]
    pub installing: bool,
}
#[derive(Default, Clone)]
struct Job {
    running: bool,
    error: Option<String>,
}
fn jobs() -> &'static std::sync::Mutex<std::collections::HashMap<HarnessId, Job>> {
    static JOBS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<HarnessId, Job>>> =
        std::sync::OnceLock::new();
    JOBS.get_or_init(Default::default)
}
pub fn start(
    h: HarnessId,
    action: InstallAction,
    complete: impl FnOnce() + Send + 'static,
) -> Result<(), HarnessError> {
    let mut states = jobs()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if states.get(&h).is_some_and(|s| s.running) {
        return Err(HarnessError::Install(
            "An installation is already in progress.".into(),
        ));
    }
    states.insert(
        h,
        Job {
            running: true,
            error: None,
        },
    );
    drop(states);
    tokio::spawn(async move {
        let result = apply(h, action).await;
        if result.is_ok() {
            complete();
        }
        jobs()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                h,
                Job {
                    running: false,
                    error: result.err().map(|e| e.to_string()),
                },
            );
    });
    Ok(())
}
async fn version(path: &Path) -> Result<String, HarnessError> {
    use tokio::io::AsyncReadExt;
    let mut command = tokio::process::Command::new(path);
    command
        .arg("--version")
        .kill_on_drop(true)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    crate::runtime_auth::RuntimeAuth::new(Vec::new(), Vec::new()).apply(&mut command);
    let mut child = command.spawn()?;
    let mut stdout = child.stdout.take().expect("piped").take(4097);
    let mut bytes = Vec::new();
    let status = tokio::time::timeout(Duration::from_secs(10), async {
        stdout.read_to_end(&mut bytes).await?;
        if bytes.len() > 4096 {
            return Err(std::io::Error::other("Version output exceeds the limit."));
        }
        child.wait().await
    })
    .await
    .map_err(|_| {
        HarnessError::Install("The executable did not respond to its version check.".into())
    })??;
    if !status.success() {
        return Err(HarnessError::Install(
            "The executable failed its version check.".into(),
        ));
    }
    let text = String::from_utf8_lossy(&bytes);
    let value = text
        .split_whitespace()
        .find(|v| {
            v.trim_start_matches('v')
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit())
        })
        .map(|v| v.trim_start_matches('v').to_owned())
        .filter(|v| v.len() < 80 && v.contains('.'))
        .ok_or_else(|| {
            HarnessError::Install("The executable returned an unrecognized version.".into())
        })?;
    Ok(value)
}
pub async fn inspect(h: HarnessId) -> Installation {
    let state = directory(h).map(|p| read(&p)).transpose();
    let (choice, previous, error) = match state {
        Ok(Some(s)) => (s.current, s.previous, None),
        Ok(None) => (None, None, None),
        Err(e) => (None, None, Some(e.to_string())),
    };
    let path = choice
        .as_ref()
        .map(|c| c.executable.clone())
        .or_else(|| if error.is_none() { discovered(h) } else { None });
    let result = match path.as_ref() {
        Some(p) => Some(version(p).await),
        None => None,
    };
    let mut row = Installation {
        harness: h,
        installed: false,
        version: None,
        managed: choice.as_ref().is_some_and(|c| c.managed),
        executable: path,
        recommended_version: recommended(h).map(str::to_owned),
        previous_version: previous.map(|p| p.version),
        can_install: h == HarnessId::Cursor
            || package(h).is_some()
            || (h == HarnessId::Devin && crate::installations_download::supports_devin()),
        error,
        installing: false,
    };
    if let Some(result) = result {
        match result {
            Ok(v) => {
                row.installed = true;
                row.version = Some(v)
            }
            Err(e) => row.error = Some(e.to_string()),
        }
    }
    if let Some(job) = jobs()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&h)
    {
        row.installing = job.running;
        if job.error.is_some() {
            row.error = job.error.clone();
        }
    }
    row
}
pub async fn list() -> Vec<Installation> {
    futures::future::join_all(HARNESSES.into_iter().map(inspect)).await
}

fn save(dir: &Path, state: &Selection) -> Result<(), HarnessError> {
    use std::io::Write;
    let temp = dir.join(format!(".selection-{}", uuid::Uuid::new_v4()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut f = options.open(&temp)?;
    f.write_all(&serde_json::to_vec(state).map_err(|e| HarnessError::Install(e.to_string()))?)?;
    f.sync_all()?;
    std::fs::rename(&temp, dir.join("selection.json"))?;
    #[cfg(unix)]
    std::fs::File::open(dir)?.sync_all()?;
    Ok(())
}

#[derive(Clone, Deserialize)]
#[serde(tag = "action", rename_all = "camelCase")]
pub enum InstallAction {
    Install,
    UseExisting { path: Option<PathBuf> },
    Rollback,
}

pub async fn apply(h: HarnessId, action: InstallAction) -> Result<Installation, HarnessError> {
    use fs2::FileExt;
    let dir = directory(h)
        .ok_or_else(|| HarnessError::Install("Cannot locate the runtime directory.".into()))?;
    std::fs::create_dir_all(&dir)?;
    let guard = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(dir.join("install.lock"))?;
    guard.try_lock_exclusive().map_err(|_| {
        HarnessError::Install("An installation is already in progress on this device.".into())
    })?;
    let mut state = match read(&dir) {
        Ok(state) => state,
        Err(error) if matches!(action, InstallAction::Rollback) => return Err(error),
        // An explicit new selection can repair a corrupt selection file. The
        // original bytes remain intact unless the new runtime passes checks.
        Err(_) => Selection::default(),
    };
    match action {
        InstallAction::Rollback => {
            let previous = state.previous.take().ok_or_else(|| {
                HarnessError::Install("No previous installation is available.".into())
            })?;
            let actual = version(&previous.executable).await?;
            if previous.managed && actual != previous.version {
                return Err(HarnessError::Install(
                    "The previous managed runtime has changed. Install the pinned version again."
                        .into(),
                ));
            }
            state.previous = state.current.replace(previous);
        }
        InstallAction::UseExisting { path } => {
            if h == HarnessId::Cursor {
                return Err(HarnessError::Install(
                    "Zeron uses the managed Cursor SDK. Install it from this page.".into(),
                ));
            }
            let path = path
                .or_else(|| discovered(h))
                .ok_or_else(|| HarnessError::NotInstalled(name(h).into()))?;
            if !path.is_absolute() {
                return Err(HarnessError::Install(
                    "Enter an absolute executable path.".into(),
                ));
            }
            let executable = std::fs::canonicalize(path)?;
            let version = version(&executable).await?;
            select(
                &mut state,
                RuntimeChoice {
                    executable,
                    version,
                    managed: false,
                },
            );
        }
        InstallAction::Install if h == HarnessId::Cursor => {
            let pin = NpmPin::parse(crate::cursor::CURSOR_SDK_PIN);
            // A content-addressed shim keeps an app update from modifying scripts
            // belonging to the currently selected runtime.
            use sha2::Digest;
            let shim_name = format!(
                "zeron-cursor-{:x}.mjs",
                sha2::Sha256::digest(crate::cursor::SHIM_SOURCE.as_bytes())
            );
            let entry = adapter_install::ensure_installed_shim(
                pin,
                "Cursor",
                &shim_name,
                crate::cursor::SHIM_SOURCE,
            )
            .await?;
            let (program, args) = adapter_install::launch_for_entry(&entry)?;
            let executable = launcher(&dir, pin.version, &program, &args)?;
            let actual = version(&executable).await?;
            if actual != pin.version {
                return Err(HarnessError::Install(
                    "Cursor returned an unexpected version.".into(),
                ));
            }
            select(
                &mut state,
                RuntimeChoice {
                    executable,
                    version: actual,
                    managed: true,
                },
            );
        }
        InstallAction::Install if h == HarnessId::Devin => {
            let executable = crate::installations_download::devin(&dir).await?;
            let actual = version(&executable).await?;
            if actual != crate::installations_download::DEVIN_VERSION {
                return Err(HarnessError::Install(
                    "Devin returned an unexpected version.".into(),
                ));
            }
            select(
                &mut state,
                RuntimeChoice {
                    executable,
                    version: actual,
                    managed: true,
                },
            );
        }
        InstallAction::Install => {
            let (pkg, bin) = package(h).ok_or_else(|| {
                HarnessError::Install("Choose an existing installation for this harness.".into())
            })?;
            let pin = NpmPin::parse(pkg);
            let entry = adapter_install::ensure_installed(pin, bin, name(h)).await?;
            let (program, args) = adapter_install::launch_for_entry(&entry)?;
            let executable = if args.is_empty() {
                program
            } else {
                launcher(&dir, pin.version, &program, &args)?
            };
            let actual = version(&executable).await?;
            if actual != pin.version {
                return Err(HarnessError::Install(format!(
                    "Expected {}, received {actual}. The previous installation is still selected.",
                    pin.version
                )));
            }
            select(
                &mut state,
                RuntimeChoice {
                    executable,
                    version: actual,
                    managed: true,
                },
            );
        }
    }
    if h == HarnessId::Pi {
        adapter_install::ensure_installed(NpmPin::parse("pi-acp@0.0.33"), "pi-acp", "Pi").await?;
    }
    save(&dir, &state)?;
    drop(guard);
    Ok(inspect(h).await)
}

fn select(state: &mut Selection, next: RuntimeChoice) {
    if state.current.as_ref() != Some(&next) {
        state.previous = state.current.replace(next);
    }
}

fn launcher(
    dir: &Path,
    release: &str,
    program: &Path,
    args: &[String],
) -> Result<PathBuf, HarnessError> {
    use sha2::Digest;
    let identity = format!("{}\0{}", program.display(), args.join("\0"));
    let fingerprint = format!("{:x}", sha2::Sha256::digest(identity.as_bytes()));
    let root = dir.join(release).join(&fingerprint[..16]);
    std::fs::create_dir_all(&root)?;
    #[cfg(windows)]
    {
        let _ = (program, args);
        return Err(HarnessError::Install(
            "This package needs a native Windows runtime.".into(),
        ));
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;
        fn quote(s: &str) -> String {
            format!("'{}'", s.replace('\'', "'\\''"))
        }
        // Retain a private Node executable so changing a version-manager alias cannot
        // change the runtime underneath a tested package. Never replace it in place.
        let node = root.join("node");
        if !node.exists() {
            let tmp = root.join(format!(".node-{}", uuid::Uuid::new_v4()));
            std::fs::copy(program, &tmp)?;
            std::fs::File::open(&tmp)?.sync_all()?;
            std::fs::rename(tmp, &node)?;
        }
        let path = root.join(dir.file_name().unwrap_or_default());
        let script = format!(
            "#!/bin/sh\nexec {} {} \"$@\"\n",
            quote(&node.to_string_lossy()),
            args.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" ")
        );
        if !path.exists() {
            let tmp = root.join(format!(".launcher-{}", uuid::Uuid::new_v4()));
            std::fs::write(&tmp, script)?;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o700))?;
            std::fs::File::open(&tmp)?.sync_all()?;
            std::fs::rename(tmp, &path)?;
            std::fs::File::open(&root)?.sync_all()?;
        }
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reselecting_current_version_keeps_the_rollback_target() {
        let old = RuntimeChoice {
            executable: "/old".into(),
            version: "1.0.0".into(),
            managed: true,
        };
        let current = RuntimeChoice {
            executable: "/current".into(),
            version: "2.0.0".into(),
            managed: true,
        };
        let mut state = Selection {
            current: Some(current.clone()),
            previous: Some(old.clone()),
        };
        select(&mut state, current.clone());
        assert_eq!(state.previous, Some(old));
        select(
            &mut state,
            RuntimeChoice {
                executable: "/new".into(),
                version: "3.0.0".into(),
                managed: true,
            },
        );
        assert_eq!(state.previous, Some(current));
    }

    #[test]
    fn selection_survives_restart_and_preserves_previous() {
        let root = tempfile::tempdir().unwrap();
        let state = Selection {
            current: Some(RuntimeChoice {
                executable: "/one".into(),
                version: "2.0.0".into(),
                managed: true,
            }),
            previous: Some(RuntimeChoice {
                executable: "/two".into(),
                version: "1.0.0".into(),
                managed: true,
            }),
        };
        save(root.path(), &state).unwrap();
        let loaded = read(root.path()).unwrap();
        assert_eq!(loaded.current.unwrap().version, "2.0.0");
        assert_eq!(loaded.previous.unwrap().version, "1.0.0");
        std::fs::write(root.path().join("selection.json"), "broken").unwrap();
        assert!(read(root.path()).is_err());
    }
}
