//! Immutable OS items. SQLite stores tagged references, never the credential itself.
use anyhow::{Context, Result, anyhow};
use ring::{
    aead,
    rand::{SecureRandom, SystemRandom},
};
use rusqlite::{Connection, OptionalExtension};
use uuid::Uuid;
use zeroize::Zeroizing;

#[cfg(target_os = "macos")]
#[path = "macos.rs"]
mod native;
#[cfg(any(target_os = "linux", target_os = "android"))]
#[path = "linux.rs"]
mod native;

const SERVICE: &str = "app.coda.cli.credentials.v1";
const REF: &[u8] = b"coda:keyring:v1:";
const SNAPSHOT: &[u8] = b"coda:snapshot:v1:";

fn reference(id: Uuid) -> Vec<u8> {
    [REF, id.as_bytes()].concat()
}

fn account(blob: &[u8]) -> Result<String> {
    let bytes = blob
        .strip_prefix(REF)
        .context("未知凭据格式；Windows DPAPI 记录须在原 Windows 用户下读取")?;
    Ok(Uuid::from_slice(bytes)
        .context("损坏的钥匙串引用")?
        .to_string())
}

/// Standalone item. Database writers use `stage` to enlist in crash recovery.
pub fn seal(value: &[u8]) -> Result<Vec<u8>> {
    let id = Uuid::new_v4();
    native::put(&id.to_string(), value)?;
    Ok(reference(id))
}

/// Release a standalone item returned by `seal`. Store-managed items follow their DB rows.
pub fn release(blob: &[u8]) -> Result<()> {
    native::remove(&account(blob)?)
}

pub fn open(blob: &[u8]) -> Result<Vec<u8>> {
    if let Some(payload) = blob.strip_prefix(SNAPSHOT) {
        return open_snapshot(payload);
    }
    native::get(&account(blob)?)
}

#[path = "journal.rs"]
mod journal;
pub(crate) use journal::{adopt_copy, reconcile, stage};

// File preimages can be much larger than an OS keyring item. Only their independent
// 256-bit data key goes into the keyring; API credentials are never wrapped this way.
pub(crate) fn seal_snapshot(db: &Connection, value: &[u8]) -> Result<Vec<u8>> {
    let key_ref: Option<Vec<u8>> = db
        .query_row("SELECT ref FROM snapshot_vault_key WHERE id=1", [], |r| {
            r.get(0)
        })
        .optional()?;
    let rng = SystemRandom::new();
    let key_ref = match key_ref {
        Some(blob) => blob,
        None => {
            let mut key = Zeroizing::new([0; 32]);
            rng.fill(key.as_mut())
                .map_err(|_| anyhow!("系统随机源不可用"))?;
            let blob = stage(db, key.as_ref())?;
            db.execute(
                "INSERT INTO snapshot_vault_key(id,ref) VALUES(1,?1)",
                [&blob],
            )?;
            blob
        }
    };
    let cipher = snapshot_cipher(&key_ref)?;
    let mut nonce = [0u8; aead::NONCE_LEN];
    rng.fill(&mut nonce)
        .map_err(|_| anyhow!("系统随机源不可用"))?;
    let mut ciphertext = value.to_vec();
    cipher
        .seal_in_place_append_tag(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::from(SNAPSHOT),
            &mut ciphertext,
        )
        .map_err(|_| anyhow!("文件快照加密失败"))?;
    Ok([SNAPSHOT, &key_ref[REF.len()..], &nonce, &ciphertext].concat())
}

fn snapshot_cipher(key_ref: &[u8]) -> Result<aead::LessSafeKey> {
    let key = Zeroizing::new(native::get(&account(key_ref)?)?);
    let key =
        aead::UnboundKey::new(&aead::AES_256_GCM, &key).map_err(|_| anyhow!("文件快照密钥无效"))?;
    Ok(aead::LessSafeKey::new(key))
}

fn open_snapshot(payload: &[u8]) -> Result<Vec<u8>> {
    let (id, rest) = payload.split_at_checked(16).context("文件快照不完整")?;
    let (nonce, body) = rest
        .split_at_checked(aead::NONCE_LEN)
        .context("文件快照不完整")?;
    let cipher = snapshot_cipher(&reference(Uuid::from_slice(id)?))?;
    let mut bytes = body.to_vec();
    let plaintext = cipher
        .open_in_place(
            aead::Nonce::try_assume_unique_for_key(nonce).map_err(|_| anyhow!("文件快照无效"))?,
            aead::Aad::from(SNAPSHOT),
            &mut bytes,
        )
        .map_err(|_| anyhow!("文件快照校验失败"))?;
    let length = plaintext.len();
    bytes.truncate(length);
    Ok(bytes)
}
