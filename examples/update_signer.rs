//! cargo run --example update_signer -- keygen
//! cargo run --example update_signer -- sign <archive> <platform>
#[allow(dead_code)]
#[path = "../src/updater/protocol.rs"]
mod protocol;

use base64::{Engine, engine::general_purpose::STANDARD};
use ring::{
    rand::SystemRandom,
    signature::{Ed25519KeyPair, KeyPair},
};
use std::{fs, io::Write, path::Path};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("keygen") => {
            // Never overwrite an existing signing identity.
            if !fs::read_to_string("update-public-key.txt")?
                .trim()
                .is_empty()
            {
                return Err("Public key already exists; keep the original private key".into());
            }
            fs::create_dir_all("target")?;
            let key = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
                .map_err(|_| "Key generation failed")?;
            let pair =
                Ed25519KeyPair::from_pkcs8(key.as_ref()).map_err(|_| "Invalid generated key")?;
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut secret = options.open("target/update-signing-key.base64")?;
            secret.write_all(STANDARD.encode(key.as_ref()).as_bytes())?;
            secret.sync_all()?;
            fs::write(
                "update-public-key.txt",
                format!("{}\n", STANDARD.encode(pair.public_key().as_ref())),
            )?;
            println!(
                "Public key written. Private key saved in target/update-signing-key.base64; keep it private and back it up."
            );
        }
        Some("sign") if args.len() == 4 => {
            let secret = STANDARD.decode(std::env::var("EDITOR_UPDATE_SIGNING_KEY")?.trim())?;
            let key = Ed25519KeyPair::from_pkcs8(&secret).map_err(|_| "Invalid signing key")?;
            if STANDARD.encode(key.public_key().as_ref()) != protocol::PUBLIC_KEY.trim() {
                return Err("Signing secret does not match update-public-key.txt".into());
            }
            let path = Path::new(&args[2]);
            let version = env!("CARGO_PKG_VERSION");
            if path.file_name().and_then(|v| v.to_str())
                != Some(&protocol::archive_name(version, &args[3]))
            {
                return Err("Archive filename does not match version/platform".into());
            }
            let manifest = protocol::Manifest {
                version: version.into(),
                platform: args[3].clone(),
                archive: path.file_name().unwrap().to_str().unwrap().into(),
                size: fs::metadata(path)?.len(),
                sha256: protocol::hash_file(path)?,
            };
            if manifest.size == 0 || manifest.size > protocol::MAX_ARCHIVE {
                return Err("Archive exceeds update size limit".into());
            }
            let bytes = serde_json::to_vec_pretty(&manifest)?;
            let filename = format!("update-{}.json", args[3]);
            let output = path.parent().unwrap().join(filename);
            fs::write(&output, &bytes)?;
            fs::write(output.with_extension("json.sig"), key.sign(&bytes).as_ref())?;
            println!("Signed {}", manifest.archive);
        }
        _ => return Err("Usage: update_signer keygen | sign <archive> <platform>".into()),
    }
    Ok(())
}
