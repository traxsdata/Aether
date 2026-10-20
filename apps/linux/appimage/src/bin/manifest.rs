// SPDX-License-Identifier: Apache-2.0
//! Sign and verify AppImage import records.

use std::env;
use std::fs;
use std::process::ExitCode;

use aether_appimage::Digest;
use aether_appimage::manifest::{self, MANIFEST_MAGIC, MANIFEST_VERSION};
use aether_packaging::signed::{TrustStore, TrustedPublisher};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("sign") => sign(&args),
        Some("verify") => verify(&args),
        _ => {
            eprintln!(
                "usage:\n  appimage-manifest sign <seed-hex-file> <image> <id> <version> <publisher> <out> [linux-appimage|linux-rootfs]\n  appimage-manifest verify <record> <image> <publisher:pubkey-hex>"
            );
            ExitCode::from(2)
        }
    }
}

fn sign(args: &[String]) -> ExitCode {
    if args.len() != 8 && args.len() != 9 {
        eprintln!(
            "usage: appimage-manifest sign <seed-hex-file> <image> <id> <version> <publisher> <out> [linux-appimage|linux-rootfs]"
        );
        return ExitCode::from(2);
    }
    let runtime = args.get(8).map_or("linux-appimage", String::as_str);
    if runtime != "linux-appimage" && runtime != "linux-rootfs" {
        eprintln!("manifest: runtime must be linux-appimage or linux-rootfs");
        return ExitCode::from(2);
    }
    let seed_text = match fs::read_to_string(&args[2]) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("manifest: {}: {error}", args[2]);
            return ExitCode::FAILURE;
        }
    };
    let mut seed = [0u8; 32];
    if aether_crypto::from_hex(seed_text.trim().as_bytes(), &mut seed) != Some(32) {
        eprintln!("manifest: seed must be 64 hexadecimal characters");
        return ExitCode::FAILURE;
    }
    let image = match fs::read(&args[3]) {
        Ok(image) => image,
        Err(error) => {
            eprintln!("manifest: {}: {error}", args[3]);
            return ExitCode::FAILURE;
        }
    };
    let digest = Digest::of(&image);
    let key = aether_crypto::ed25519::public_key_from_seed(&seed);
    let mut digest_hex = [0u8; 64];
    let digest_len = aether_crypto::to_hex(digest.as_bytes(), &mut digest_hex).unwrap();
    let mut key_hex = [0u8; 64];
    let key_len = aether_crypto::to_hex(&key, &mut key_hex).unwrap();
    let body = format!(
        "id={}\nversion={}\nsha256={}\nruntime={}\npublisher={}\npublisher-key={}\n",
        args[4],
        args[5],
        core::str::from_utf8(&digest_hex[..digest_len]).unwrap(),
        runtime,
        args[6],
        core::str::from_utf8(&key_hex[..key_len]).unwrap()
    );
    let signature = aether_crypto::ed25519::sign(&seed, body.as_bytes());
    let mut record = Vec::new();
    record.extend_from_slice(&MANIFEST_MAGIC);
    record.extend_from_slice(&MANIFEST_VERSION.to_le_bytes());
    record.extend_from_slice(&(body.len() as u32).to_le_bytes());
    record.extend_from_slice(body.as_bytes());
    record.extend_from_slice(&signature);
    if let Err(error) = fs::write(&args[7], record) {
        eprintln!("manifest: {}: {error}", args[7]);
        return ExitCode::FAILURE;
    }
    println!(
        "signed id={} version={} runtime={} publisher={} out={}",
        args[4], args[5], runtime, args[6], args[7]
    );
    ExitCode::SUCCESS
}

fn verify(args: &[String]) -> ExitCode {
    if args.len() != 5 {
        eprintln!("usage: appimage-manifest verify <record> <image> <publisher:pubkey-hex>");
        return ExitCode::from(2);
    }
    let record = match fs::read(&args[2]) {
        Ok(record) => record,
        Err(error) => {
            eprintln!("manifest: {}: {error}", args[2]);
            return ExitCode::FAILURE;
        }
    };
    let image = match fs::read(&args[3]) {
        Ok(image) => image,
        Err(error) => {
            eprintln!("manifest: {}: {error}", args[3]);
            return ExitCode::FAILURE;
        }
    };
    let Some((name, key_text)) = args[4].split_once(':') else {
        eprintln!("manifest: trust must be <publisher:pubkey-hex>");
        return ExitCode::from(2);
    };
    let mut key = [0u8; 32];
    if aether_crypto::from_hex(key_text.as_bytes(), &mut key) != Some(32) {
        eprintln!("manifest: publisher key must be 64 hexadecimal characters");
        return ExitCode::FAILURE;
    }
    let entries = [TrustedPublisher { name, key }];
    let trust = TrustStore::new(&entries);
    match manifest::verify_image(&record, &trust, &image) {
        Ok(record) => {
            println!(
                "verified id={} version={} publisher={} digest=matched",
                record.id, record.version, record.publisher
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("manifest: {error:?}");
            ExitCode::FAILURE
        }
    }
}
