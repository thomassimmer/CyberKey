use std::sync::{Arc, Mutex};

use esp_idf_svc::nvs::{EspNvs, EspNvsPartition, NvsEncrypted};

/// Newtype that lets `EspNvs` cross thread boundaries under a `Mutex`.
pub struct SharedNvs(pub EspNvs<NvsEncrypted>);
// Safety: access is serialised by the surrounding Mutex.
unsafe impl Send for SharedNvs {}

/// Initialise the encrypted NVS partition and return the shared handle.
///
/// If existing unencrypted data prevents secure init, the partition is erased
/// and re-initialised (this only happens on first boot after a flash erase).
pub fn init() -> anyhow::Result<Arc<Mutex<SharedNvs>>> {
    let nvs_partition = match EspNvsPartition::<NvsEncrypted>::take("nvs", Some("nvs_keys")) {
        Ok(p) => p,
        Err(e) => {
            log::warn!(
                "Encrypted NVS init failed ({:?}), erasing partition and retrying",
                e
            );
            unsafe { esp_idf_svc::sys::nvs_flash_erase_partition(c"nvs".as_ptr()) };
            EspNvsPartition::<NvsEncrypted>::take("nvs", Some("nvs_keys"))?
        }
    };
    let nvs_inner = EspNvs::new(nvs_partition, "ck", true)?;
    Ok(Arc::new(Mutex::new(SharedNvs(nvs_inner))))
}
