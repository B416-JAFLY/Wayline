//! Both front ends publish one stable identity and retire the same renamed services.
use anyhow::{Context, Result, ensure};
use std::{fs::File, io::Read, path::Path};
use wayline_core::Identity;

pub struct ReceiverAdvertisement {
    pub service_id: String,
    pub retired_service_ids: Vec<String>,
}

pub fn receiver_advertisement(identity: &Identity, name: &str) -> Result<ReceiverAdvertisement> {
    let name = super::normalize_name(name)?;
    let service_id = identity.named_service_id(&name);
    let retired_service_ids = remember(
        &super::identity_dir()?.join("receiver-services.json"),
        &service_id,
        &[
            identity.service_id(),
            identity.named_service_id(&super::default_name()),
        ],
    )?;
    Ok(ReceiverAdvertisement {
        service_id,
        retired_service_ids,
    })
}

fn remember(path: &Path, current: &str, legacy: &[String]) -> Result<Vec<String>> {
    let _guard = super::FileGuard::acquire(&path.with_extension("lock"))?;
    let previous = match File::open(path) {
        Ok(file) => {
            let mut bytes = Vec::new();
            file.take(4097).read_to_end(&mut bytes)?;
            ensure!(bytes.len() <= 4096, "接收身份历史文件过大");
            serde_json::from_slice(&bytes).context("接收身份历史格式不正确")?
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    let retired = retired_services(previous, current, legacy);
    let mut stored = retired.clone();
    stored.push(current.to_owned());
    let mut temporary = tempfile::NamedTempFile::new_in(path.parent().context("无效的身份目录")?)?;
    serde_json::to_writer(&mut temporary, &stored)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).context("无法保存接收身份历史")?;
    Ok(retired)
}

fn retired_services(previous: Vec<String>, current: &str, legacy: &[String]) -> Vec<String> {
    let mut retired = Vec::new();
    for id in legacy.iter().cloned().chain(previous) {
        if id.len() == 12
            && id.bytes().all(|b| b.is_ascii_hexdigit())
            && !id.eq_ignore_ascii_case(current)
        {
            let id = id.to_ascii_lowercase();
            retired.retain(|old| old != &id);
            retired.push(id);
        }
    }
    if retired.len() > 8 {
        retired.drain(..retired.len() - 8);
    }
    retired
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontends_share_renames_without_retiring_the_active_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("receiver-services.json");
        let old = "111111111111";
        let new = "222222222222";
        let legacy = ["333333333333".to_owned()];
        assert_eq!(remember(&path, old, &legacy).unwrap(), legacy);
        assert_eq!(
            remember(&path, new, &legacy).unwrap(),
            [legacy[0].clone(), old.into()]
        );
        assert_eq!(
            remember(&path, new, &legacy).unwrap(),
            [legacy[0].clone(), old.into()]
        );
        assert_eq!(
            remember(&path, old, &legacy).unwrap(),
            [legacy[0].clone(), new.into()]
        );
        let many = (0..30)
            .map(|n| format!("{n:012x}"))
            .chain(["not-a-service".into()])
            .collect();
        let retired = retired_services(many, "000000000029", &legacy);
        assert_eq!(retired.len(), 8);
        assert!(
            !retired
                .iter()
                .any(|id| id == "000000000029" || id == "not-a-service")
        );
    }

    #[test]
    fn corrupt_history_is_preserved_for_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("receiver-services.json");
        for bytes in [b"{".to_vec(), vec![b' '; 4097]] {
            std::fs::write(&path, &bytes).unwrap();
            assert!(remember(&path, "111111111111", &[]).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
    }
}
