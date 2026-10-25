//! Shared bounded I/O and atomic publication for generated directory packages.
use anyhow::{bail, ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

pub fn sha256(path: &Path) -> Result<String> {
    let mut source = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let count = source.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub fn json(path: &Path) -> Result<serde_json::Value> {
    const LIMIT: u64 = 4 * 1024 * 1024;
    let mut bytes = Vec::new();
    File::open(path)
        .with_context(|| format!("open {}", path.display()))?
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= LIMIT,
        "JSON is too large: {}",
        path.display()
    );
    serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))
}

pub fn string<'a>(value: &'a serde_json::Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|text| !text.is_empty())
        .with_context(|| format!("missing string field {key}"))
}

pub fn relative_path(name: &str) -> Result<PathBuf> {
    let path = Path::new(name);
    ensure!(
        !name.is_empty()
            && !name.contains([':', '\\', '\0'])
            && path
                .components()
                .all(|part| matches!(part, Component::Normal(_))),
        "unsafe relative path: {name}"
    );
    Ok(path.to_owned())
}

pub fn regular(path: &Path) -> Result<()> {
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("inspect {}", path.display()))?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "not a regular file: {}",
        path.display()
    );
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        ensure!(
            metadata.file_attributes() & 0x400 == 0,
            "reparse points are not packaged: {}",
            path.display()
        );
    }
    Ok(())
}

pub fn source_file(root: &Path, name: &str) -> Result<PathBuf> {
    let relative = relative_path(name)?;
    let root = fs::canonicalize(root)
        .with_context(|| format!("resolve input directory {}", root.display()))?;
    let path = root.join(relative);
    regular(&path)?;
    let resolved = fs::canonicalize(&path)
        .with_context(|| format!("resolve input file {}", path.display()))?;
    ensure!(
        resolved.starts_with(&root),
        "input escapes its directory: {name}"
    );
    Ok(resolved)
}

pub fn copy(source: &Path, target: &Path) -> Result<()> {
    regular(source)?;
    fs::create_dir_all(target.parent().context("output has no parent")?)?;
    fs::copy(source, target)
        .with_context(|| format!("copy {} to {}", source.display(), target.display()))?;
    Ok(())
}

pub fn write_json(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    let mut file = File::create(path)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    Ok(())
}

pub fn publish(output: &Path, build: impl FnOnce(&Path) -> Result<()>) -> Result<()> {
    ensure_absent(output)?;
    let parent = output.parent().context("output has no parent")?;
    fs::create_dir_all(parent)?;
    let stage = tempfile::Builder::new()
        .prefix(".wayline-package-")
        .tempdir_in(parent)?;
    build(stage.path())?;
    ensure_absent(output)?;
    fs::rename(stage.path(), output).with_context(|| format!("publish {}", output.display()))?;
    Ok(())
}

fn ensure_absent(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => bail!("output already exists: {}", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("inspect output {}", path.display())),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn dangling_output_symlink_is_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("output");
        std::os::unix::fs::symlink(dir.path().join("missing"), &output).unwrap();
        assert!(publish(&output, |_| Ok(())).is_err());
        assert!(fs::symlink_metadata(output)
            .unwrap()
            .file_type()
            .is_symlink());
    }
}

pub fn release_manifest(root: &Path, platform: &str, version: &str) -> Result<()> {
    fn walk(root: &Path, directory: &Path, entries: &mut Vec<serde_json::Value>) -> Result<()> {
        for entry in fs::read_dir(directory)? {
            let path = entry?.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                bail!("symlink in package: {}", path.display());
            }
            if metadata.is_dir() {
                walk(root, &path, entries)?;
            } else {
                regular(&path)?;
                entries.push(serde_json::json!({"path":path.strip_prefix(root)?.to_string_lossy().replace('\\', "/"),"length":metadata.len(),"sha256":sha256(&path)?}));
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    walk(root, root, &mut files)?;
    files.sort_by(|left, right| left["path"].as_str().cmp(&right["path"].as_str()));
    write_json(
        &root.join("release.json"),
        &serde_json::json!({"schemaVersion":1,"platform":platform,"version":version,"files":files}),
    )
}
