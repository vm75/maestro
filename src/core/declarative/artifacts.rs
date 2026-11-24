//! Shared private artifact publication for native declarative providers.
use std::{fs::File,path::Path};
use anyhow::{Result,anyhow,ensure};
use rustix::{fs::{self as unix_fs,AtFlags,Mode},io::Errno};
use super::store;

fn optional<T>(result: Result<T>) -> Result<Option<T>> {
    match result {
        Ok(value)=>Ok(Some(value)),
        Err(error) if error.downcast_ref::<Errno>()==Some(&Errno::NOENT)=>Ok(None),
        Err(error)=>Err(error),
    }
}

pub(super) fn read_artifact(directory: &Path,name: &str) -> Result<Option<Vec<u8>>> {
    let Some(dir)=optional(store::open_directory(directory))? else{return Ok(None)};
    optional(store::read_private(&dir,name,super::MAX_CONFIG_BYTES))
}

pub(super) fn check_bytes(actual: Option<&[u8]>,before: Option<&[u8]>,after: Option<&[u8]>) -> Result<()> {
    ensure!(actual==before || actual==after,"generated artifact differs from the reviewed before and after definitions");
    Ok(())
}

// Open every newly created directory relative to its trusted parent. The
// existing store helpers reject symlinks and foreign or non-private ownership.
fn ensure_dir(path: &Path) -> Result<File> {
    if let Some(dir)=optional(store::open_directory(path))? {return Ok(dir);}
    let parent=path.parent().ok_or_else(||anyhow!("missing artifact parent"))?;
    let name=path.file_name().and_then(|n|n.to_str()).ok_or_else(||anyhow!("invalid artifact name"))?;
    let dir=ensure_dir(parent)?;
    match unix_fs::mkdirat(&dir,name,Mode::from_bits_truncate(0o700)) {
        Ok(())=>dir.sync_all()?,Err(Errno::EXIST)=>{},Err(error)=>return Err(error.into()),
    }
    store::open_child_directory(&dir,name)
}

pub(super) fn publish_artifact(directory: &Path,name: &str,before: Option<&[u8]>,after: Option<&[u8]>) -> Result<()> {
    let dir=if after.is_some(){ensure_dir(directory)?}else{
        let Some(dir)=optional(store::open_directory(directory))? else {return Ok(())};
        dir
    };
    let lock=store::LockedDirectory::try_acquire(dir)?;
    let actual=optional(store::read_private(&lock,name,super::MAX_CONFIG_BYTES))?;
    check_bytes(actual.as_deref(),before,after)?;
    if actual.as_deref()==after{return Ok(())}
    if let Some(data)=after {store::publish(&lock,name,data,actual.is_none())?;}
    else {unix_fs::unlinkat(&lock,name,AtFlags::empty())?;lock.sync_all()?;}
    Ok(())
}

