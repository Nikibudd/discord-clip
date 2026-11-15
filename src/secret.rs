//! The soundboard service's upload key (#76), kept encrypted for this
//! Windows user with DPAPI in `soundboard-key.bin`, never in the config file.

use std::path::PathBuf;

use anyhow::{Context, Result};
use windows::Win32::Foundation::{LocalFree, HLOCAL};
use windows::Win32::Security::Cryptography::{
    CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
};

fn path() -> PathBuf {
    crate::config::app_dir().join("soundboard-key.bin")
}

/// The saved upload key, if any.
pub fn upload_key() -> Option<String> {
    let data = std::fs::read(path()).ok()?;
    match unprotect(&data) {
        Ok(plain) => String::from_utf8(plain).ok().filter(|k| !k.is_empty()),
        Err(e) => {
            crate::log!("reading the soundboard key failed: {e:#}");
            None
        }
    }
}

/// Saves the upload key, or removes it when `key` is empty.
pub fn set_upload_key(key: &str) -> Result<()> {
    let path = path();
    if key.is_empty() {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e).context("removing the key"),
            _ => Ok(()),
        };
    }
    std::fs::create_dir_all(crate::config::app_dir())?;
    std::fs::write(&path, protect(key.as_bytes())?).with_context(|| format!("writing {}", path.display()))
}

fn protect(plain: &[u8]) -> Result<Vec<u8>> {
    let input = CRYPT_INTEGER_BLOB { cbData: plain.len() as u32, pbData: plain.as_ptr() as *mut u8 };
    let mut out = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptProtectData(&input, None, None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out)
            .context("encrypting the key")?;
        Ok(take(out))
    }
}

fn unprotect(data: &[u8]) -> Result<Vec<u8>> {
    let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
    let mut out = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptUnprotectData(&input, None, None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out)
            .context("decrypting the key")?;
        Ok(take(out))
    }
}

/// Copies a blob DPAPI allocated and frees it.
unsafe fn take(blob: CRYPT_INTEGER_BLOB) -> Vec<u8> {
    let v = std::slice::from_raw_parts(blob.pbData, blob.cbData as usize).to_vec();
    let _ = LocalFree(HLOCAL(blob.pbData as *mut _));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let secret = protect(b"hello key").unwrap();
        assert_ne!(secret, b"hello key");
        assert_eq!(unprotect(&secret).unwrap(), b"hello key");
    }
}
