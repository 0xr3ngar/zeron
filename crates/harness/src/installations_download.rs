//! Pinned native bundles. Verify the entire download before extracting or selecting it.
use crate::HarnessError;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
pub const DEVIN_VERSION: &str = "3000.10.21";
fn devin_release() -> Option<(&'static str, &'static str)> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some((
            "x86_64-unknown-linux",
            "7cac6f5739ba3a3e5542f3b7fa07ed902d6dfb96ca22e4c63ae84c03bb7db47c",
        )),
        ("linux", "aarch64") => Some((
            "aarch64-unknown-linux",
            "a63124ed2f8406a5d44a162fa2eb05b9c0f218a6b131e2ca1335d4a335c70a6c",
        )),
        ("macos", "x86_64") => Some((
            "x86_64-apple-darwin",
            "4725d6b0dbbf6f71d833b5489469dc8b5c4a4f929926f94d500952a4cb7bbad8",
        )),
        ("macos", "aarch64") => Some((
            "aarch64-apple-darwin",
            "c0b97f8197bf3ce895ff14aa19257c511154b49a0a195bba4962acb5e475c68e",
        )),
        _ => None,
    }
}
pub fn supports_devin() -> bool {
    devin_release().is_some()
}
pub async fn devin(dir: &Path) -> Result<PathBuf, HarnessError> {
    use futures::StreamExt;
    use std::io::Write;
    let (target, checksum) = devin_release().ok_or_else(|| {
        HarnessError::Install("Use an existing Devin installation on this platform.".into())
    })?;
    let destination = dir.join(DEVIN_VERSION);
    if destination.join(".verified").is_file() {
        return Ok(destination.join("bin/devin"));
    }
    let mut download = tempfile::NamedTempFile::new_in(dir)?;
    let client = reqwest::Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .map_err(|_| HarnessError::Install("Could not initialize the runtime download.".into()))?;
    let response = client
        .get(format!(
            "https://static.devin.ai/cli/{DEVIN_VERSION}/devin-{DEVIN_VERSION}-{target}.tar.gz"
        ))
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|_| {
            HarnessError::Install(
                "Could not download Devin. Check your connection and retry.".into(),
            )
        })?;
    let mut stream = response.bytes_stream();
    let mut digest = Sha256::new();
    let mut bytes = 0usize;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| {
            HarnessError::Install("Devin download interrupted. Retry the installation.".into())
        })?;
        bytes = bytes.saturating_add(chunk.len());
        if bytes > 512 * 1024 * 1024 {
            return Err(HarnessError::Install(
                "Runtime download exceeds the size limit.".into(),
            ));
        }
        digest.update(&chunk);
        download.write_all(&chunk)?;
    }
    if format!("{:x}", digest.finalize()) != checksum {
        return Err(HarnessError::Install(
            "Devin's checksum did not match the pinned release. No runtime was changed.".into(),
        ));
    }
    download.as_file().sync_all()?;
    let dir = dir.to_owned();
    tokio::task::spawn_blocking(move || {
        let stage = tempfile::Builder::new()
            .prefix(".install-")
            .tempdir_in(&dir)?;
        let archive = std::fs::File::open(download.path())?;
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(archive));
        let mut expanded = 0u64;
        let mut entries = 0usize;
        for entry in archive.entries()? {
            let mut entry = entry?;
            entries += 1;
            expanded = expanded.saturating_add(entry.size());
            if expanded > 2 * 1024 * 1024 * 1024 || entries > 20_000 {
                return Err(HarnessError::Install(
                    "Runtime archive exceeds the extraction limit.".into(),
                ));
            }
            let kind = entry.header().entry_type();
            if !kind.is_file() && !kind.is_dir() {
                return Err(HarnessError::Install(
                    "Runtime archive contains an unsupported link or special file.".into(),
                ));
            }
            if !entry.unpack_in(stage.path())? {
                return Err(HarnessError::Install(
                    "Runtime archive contains an unsafe path.".into(),
                ));
            }
        }
        if !stage.path().join("bin/devin").is_file() {
            return Err(HarnessError::Install(
                "Runtime archive is missing its executable.".into(),
            ));
        }
        // Flush extracted files before the directory becomes an install candidate.
        fn sync_tree(path: &Path) -> std::io::Result<()> {
            for item in std::fs::read_dir(path)? {
                let p = item?.path();
                if p.is_dir() {
                    sync_tree(&p)?;
                } else {
                    std::fs::File::open(p)?.sync_all()?;
                }
            }
            #[cfg(unix)]
            std::fs::File::open(path)?.sync_all()?;
            Ok(())
        }
        sync_tree(stage.path())?;
        std::fs::write(stage.path().join(".verified"), checksum)?;
        std::fs::File::open(stage.path().join(".verified"))?.sync_all()?;
        std::fs::rename(stage.path(), &destination)?;
        #[cfg(unix)]
        std::fs::File::open(&dir)?.sync_all()?;
        Ok(destination.join("bin/devin"))
    })
    .await
    .map_err(|_| HarnessError::Install("Runtime extraction did not finish.".into()))?
}
