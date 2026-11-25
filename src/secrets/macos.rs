use super::SERVICE;
use anyhow::{Result, anyhow, ensure};
use core_foundation::base::{CFTypeRef, TCFType};
use security_framework::{
    item::{ItemClass, ItemSearchOptions},
    os::macos::keychain::SecKeychain,
    passwords::{delete_generic_password, get_generic_password},
};
use std::path::Path;

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    fn SecKeychainGetStatus(keychain: CFTypeRef, status: *mut u32) -> i32;
}

fn explicit_keychain() -> Result<Option<SecKeychain>> {
    let Some(path) = std::env::var_os("CODA_KEYCHAIN_PATH") else {
        return Ok(None);
    };
    ensure!(
        Path::new(&path).is_absolute() && Path::new(&path).is_file(),
        "CODA_KEYCHAIN_PATH must specify an existing absolute Keychain file"
    );
    let keychain = SecKeychain::open(path)?;
    let mut status = 0;
    // SAFETY: `keychain` retains a SecKeychainRef during the call; status is a writable UInt32.
    let code = unsafe { SecKeychainGetStatus(keychain.as_CFTypeRef(), &mut status) };
    const UNLOCKED: u32 = 1; // kSecUnlockStateStatus
    ensure!(
        code == 0 && status & UNLOCKED != 0,
        "Explicit macOS Keychain is locked or unavailable"
    );
    Ok(Some(keychain))
}

pub(super) fn put(account: &str, value: &[u8]) -> Result<()> {
    let keychain = match explicit_keychain()? {
        Some(chain) => chain,
        None => SecKeychain::default()?,
    };
    keychain.add_generic_password(SERVICE, account, value)?;
    Ok(())
}

pub(super) fn get(account: &str) -> Result<Vec<u8>> {
    let result = match explicit_keychain()? {
        Some(chain) => chain
            .find_generic_password(SERVICE, account)
            .map(|(password, _)| password.to_vec()),
        None => get_generic_password(SERVICE, account),
    };
    result.map_err(|error| anyhow!("macOS Keychain 读取失败 ({})", error.code()))
}

pub(super) fn remove(account: &str) -> Result<()> {
    let result = match explicit_keychain()? {
        Some(chain) => ItemSearchOptions::new()
            .keychains(&[chain])
            .class(ItemClass::generic_password())
            .service(SERVICE)
            .account(account)
            .delete(),
        None => delete_generic_password(SERVICE, account),
    };
    match result {
        Ok(()) => Ok(()),
        Err(error) if error.code() == -25300 => Ok(()), // errSecItemNotFound
        Err(error) => Err(anyhow!("macOS Keychain 删除失败 ({})", error.code())),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "requires CODA_KEYCHAIN_PATH pointing to a missing test keychain"]
    fn missing_explicit_keychain_rejects_removal() {
        let path = std::env::var_os("CODA_KEYCHAIN_PATH").unwrap();
        assert!(!std::path::Path::new(&path).exists());
        assert!(super::remove(&uuid::Uuid::new_v4().to_string()).is_err());
    }
}
