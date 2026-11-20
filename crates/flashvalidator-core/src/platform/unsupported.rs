//! Fallback for operating systems without a raw-I/O backend.

use crate::blockdev::{Access, BlockDevice};
use crate::device::DeviceInfo;
use crate::error::{Error, Result};

pub(crate) fn enumerate() -> Result<Vec<DeviceInfo>> {
    Ok(Vec::new())
}

pub(crate) fn open(_info: &DeviceInfo, _access: Access) -> Result<Box<dyn BlockDevice>> {
    Err(Error::Unsupported)
}
