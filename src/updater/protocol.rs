//! Shared by the editor and the release-signing example.
use base64::{Engine, engine::general_purpose::STANDARD};
use ring::{digest, signature};
use serde::{Deserialize, Serialize};
use std::{fs::File, io::Read, path::Path};

pub const PUBLIC_KEY: &str = include_str!("../../update-public-key.txt");
pub const MAX_ARCHIVE: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: String,
    pub platform: String,
    pub archive: String,
    pub size: u64,
    pub sha256: String,
}

pub fn hash_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let mut hash = digest::Context::new(&digest::SHA256);
    let mut buffer = [0; 64 * 1024];
    loop {
        let n = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(hash
        .finish()
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

pub fn verify(bytes: &[u8], signature: &[u8], key: &str) -> Result<Manifest, String> {
    let key = STANDARD
        .decode(key.trim())
        .map_err(|_| "Invalid update public key")?;
    signature::UnparsedPublicKey::new(&signature::ED25519, key)
        .verify(bytes, signature)
        .map_err(|_| "Update signature verification failed")?;
    serde_json::from_slice(bytes).map_err(|e| format!("Invalid signed manifest: {e}"))
}

pub fn archive_name(version: &str, platform: &str) -> String {
    let extension = if platform == "linux-x86_64" {
        "tar.gz"
    } else {
        "zip"
    };
    format!("editor-{version}-{platform}.{extension}")
}

pub fn validate(manifest: &Manifest, version: &str, platform: &str) -> Result<(), String> {
    let parsed = semver::Version::parse(version).map_err(|e| e.to_string())?;
    if !parsed.pre.is_empty()
        || manifest.version != version
        || manifest.platform != platform
        || manifest.archive != archive_name(version, platform)
        || manifest.size == 0
        || manifest.size > MAX_ARCHIVE
    {
        return Err("Update does not match the requested version or platform".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::{
        rand::SystemRandom,
        signature::{Ed25519KeyPair, KeyPair},
    };

    #[test]
    fn signatures_reject_tampering_and_other_keys() {
        let key = Ed25519KeyPair::from_pkcs8(
            Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
                .unwrap()
                .as_ref(),
        )
        .unwrap();
        let public = STANDARD.encode(key.public_key().as_ref());
        let bytes = br#"{"version":"1.0.0","platform":"windows-x86_64","archive":"editor-1.0.0-windows-x86_64.zip","size":100,"sha256":"abc"}"#;
        let signed = key.sign(bytes);
        assert!(verify(bytes, signed.as_ref(), &public).is_ok());
        let mut altered = bytes.to_vec();
        altered[12] = b'9';
        assert!(verify(&altered, signed.as_ref(), &public).is_err());
        assert!(verify(bytes, signed.as_ref(), &STANDARD.encode([0; 32])).is_err());
        let mut manifest = verify(bytes, signed.as_ref(), &public).unwrap();
        assert!(validate(&manifest, "1.0.0", "windows-x86_64").is_ok());
        assert!(validate(&manifest, "1.0.0", "macos-aarch64").is_err());
        manifest.archive = "../../editor.exe".into();
        assert!(validate(&manifest, "1.0.0", "windows-x86_64").is_err());
    }
}
