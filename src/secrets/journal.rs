//! Durable ownership of Keychain items. The SQLite writer lock of the main database
//! covers creation and collection. The independent file survives a main-DB rollback.
use super::{REF, SNAPSHOT, account, native, reference};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use std::{
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::PathBuf,
};
use uuid::Uuid;
use zeroize::Zeroizing;

struct Journal {
    db: Connection,
    owner: [u8; 32],
}

impl Journal {
    fn open(main: &Connection, create: bool) -> Result<Option<Self>> {
        let path = PathBuf::from(main.path().context("凭据需要持久化数据库")?).canonicalize()?;
        let sidecar = path.with_added_extension("credential-journal");
        if !create && !sidecar.try_exists()? {
            return Ok(None);
        }
        let meta = path.metadata()?;
        let owner = Sha256::new()
            .chain_update(meta.dev().to_be_bytes())
            .chain_update(meta.ino().to_be_bytes())
            .chain_update(path.as_os_str().as_bytes())
            .finalize()
            .into();
        let db = Connection::open(sidecar)?;
        db.execute_batch("PRAGMA synchronous=FULL; CREATE TABLE IF NOT EXISTS owned(owner BLOB NOT NULL,ref BLOB NOT NULL,PRIMARY KEY(owner,ref))")?;
        Ok(Some(Self { db, owner }))
    }

    fn owns(&self, blob: &[u8]) -> Result<bool> {
        Ok(self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM owned WHERE owner=?1 AND ref=?2)",
            params![self.owner, blob],
            |r| r.get(0),
        )?)
    }
}

pub(crate) fn stage(main: &Connection, value: &[u8]) -> Result<Vec<u8>> {
    ensure!(!main.is_autocommit(), "凭据写入需要事务");
    let journal = Journal::open(main, true)?.context("缺少凭据日志")?;
    let id = Uuid::new_v4();
    let blob = reference(id);
    journal.db.execute(
        "INSERT INTO owned(owner,ref) VALUES(?1,?2)",
        params![journal.owner, blob],
    )?;
    native::put(&id.to_string(), value)?;
    Ok(blob)
}

pub(crate) fn reconcile(main: &mut Connection) {
    if collect(main).is_err() {
        eprintln!("系统钥匙串清理尚未完成，下次打开数据库时将重试");
    }
}

fn collect(main: &mut Connection) -> Result<()> {
    let Some(journal) = Journal::open(main, false)? else {
        return Ok(());
    };
    let tx = main.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut stmt = journal.db.prepare("SELECT ref FROM owned WHERE owner=?1")?;
    let entries = stmt
        .query_map([journal.owner], |r| r.get::<_, Vec<u8>>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    let mut live = tx.prepare("SELECT EXISTS(SELECT 1 FROM secrets WHERE value=?1 UNION ALL SELECT 1 FROM snapshot_vault_key WHERE ref=?1)")?;
    let mut failed = false;
    for blob in entries {
        if !live.query_row([&blob], |r| r.get::<_, bool>(0))? {
            if account(&blob).and_then(|id| native::remove(&id)).is_err() {
                failed = true;
                continue;
            }
            journal.db.execute(
                "DELETE FROM owned WHERE owner=?1 AND ref=?2",
                params![journal.owner, blob],
            )?;
        }
    }
    drop(live);
    tx.commit()?;
    ensure!(!failed, "部分钥匙串条目仍待清理");
    Ok(())
}

// Copies may initially read the original items. Duplicate readable items before allowing
// rotation on either database. A missing/locked item stays replaceable via normal config.
pub(crate) fn adopt_copy(tx: &Transaction<'_>) -> Result<()> {
    let journal = Journal::open(tx, false)?;
    let mut stmt = tx.prepare("SELECT id,value FROM secrets")?;
    let entries = stmt
        .query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    let duplicate = |blob: &[u8]| -> Result<Option<Vec<u8>>> {
        let Ok(id) = account(blob) else {
            return Ok(None);
        };
        if let Some(journal) = &journal
            && journal.owns(blob)?
        {
            return Ok(None);
        }
        let Ok(value) = native::get(&id) else {
            return Ok(None);
        };
        let value = Zeroizing::new(value);
        Ok(Some(stage(tx, &value)?))
    };
    for (id, blob) in entries {
        if let Some(new) = duplicate(&blob)? {
            tx.execute("UPDATE secrets SET value=?1 WHERE id=?2", params![new, id])?;
        }
    }
    let snapshot: Option<Vec<u8>> = tx
        .query_row("SELECT ref FROM snapshot_vault_key WHERE id=1", [], |r| {
            r.get(0)
        })
        .optional()?;
    if let Some(old) = snapshot
        && let Some(new) = duplicate(&old)?
    {
        let old_header = [SNAPSHOT, &old[REF.len()..]].concat();
        let new_header = [SNAPSHOT, &new[REF.len()..]].concat();
        // The new keyring item contains the same data key, so only its locator changes.
        tx.execute("UPDATE workspace_changes SET before_blob=CAST(?1 || substr(before_blob,?2+1) AS BLOB) WHERE substr(before_blob,1,?2)=?3", params![new_header,old_header.len(),old_header])?;
        tx.execute("UPDATE snapshot_vault_key SET ref=?1 WHERE id=1", [new])?;
    }
    Ok(())
}
