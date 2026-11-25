use super::SERVICE;
use anyhow::{Result, anyhow, ensure};
use secret_service::{
    EncryptionType,
    blocking::{Item, SecretService},
};
use std::collections::HashMap;

fn unavailable(_: secret_service::Error) -> anyhow::Error {
    anyhow!("Linux Secret Service 不可用，请检查用户 D-Bus 会话并解锁系统钥匙环")
}

fn connect() -> Result<SecretService<'static>> {
    SecretService::connect(EncryptionType::Dh).map_err(unavailable)
}

fn attributes(account: &str) -> HashMap<&str, &str> {
    HashMap::from([("service", SERVICE), ("account", account)])
}

pub(super) fn put(account: &str, value: &[u8]) -> Result<()> {
    let service = connect()?;
    let collection = service.get_default_collection().map_err(unavailable)?;
    collection.ensure_unlocked().map_err(unavailable)?;
    collection
        .create_item(
            "Coda CLI",
            attributes(account),
            value,
            false,
            "application/octet-stream",
        )
        .map_err(unavailable)?;
    Ok(())
}

fn find<'a>(service: &'a SecretService<'a>, account: &str) -> Result<Option<Item<'a>>> {
    let result = service
        .search_items(attributes(account))
        .map_err(unavailable)?;
    ensure!(result.locked.is_empty(), "请解锁保存凭据的系统钥匙环");
    ensure!(result.unlocked.len() <= 1, "系统钥匙环中存在重复凭据引用");
    Ok(result.unlocked.into_iter().next())
}

pub(super) fn get(account: &str) -> Result<Vec<u8>> {
    find(&connect()?, account)?
        .ok_or_else(|| anyhow!("系统钥匙环中找不到凭据"))?
        .get_secret()
        .map_err(unavailable)
}

pub(super) fn remove(account: &str) -> Result<()> {
    if let Some(item) = find(&connect()?, account)? {
        item.delete().map_err(unavailable)?;
    }
    Ok(())
}
