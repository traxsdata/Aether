// SPDX-License-Identifier: Apache-2.0
//! Developer tooling for package and update signing.
//!
//! This binary is the only component that handles private keys. It is a host
//! tool: the kernel links the verifier, not the signer. Keys are read from a
//! hex file, never from the repository, the environment, or a command line
//! argument, so a key cannot leak through shell history or a process listing.
//!
//! Build tooling canonicalises manifests through `aether-packaging` and
//! `aether-update`, so a signed artifact is byte-for-byte what the kernel
//! parses.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use aether_crypto::{ed25519, from_hex, sha256, to_hex};
use aether_packaging::manifest::{Channel, ManifestFields, Version, is_safe_relative_path};
use aether_packaging::payload::encode_payload;
use aether_packaging::signed::{TrustStore, TrustedPublisher, encode_frame};
use aether_packaging::{MAX_MANIFEST_BYTES, PACKAGE_FORMAT, PACKAGE_MAGIC};
use aether_update::{UPDATE_FORMAT, UPDATE_MAGIC, UpdateManifest, check_policy, verify_update};

type Result<T> = std::result::Result<T, String>;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("aether-admin: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<()> {
    match args {
        [help] if help == "--help" || help == "-h" || help == "help" => {
            println!("{}", usage());
            Ok(())
        }
        [group, command, rest @ ..] => match (group.as_str(), command.as_str()) {
            ("key", "public") => key_public(rest),
            ("package", "build") => package_build(rest),
            ("package", "verify") => package_verify(rest),
            ("update", "sign") => update_sign(rest),
            ("update", "verify") => update_verify(rest),
            ("update", "policy") => update_policy(rest),
            ("release", "build") => release_build(rest),
            ("release", "verify") => release_verify(rest),
            _ => Err(format!("unknown command '{group} {command}'")),
        },
        [] => Err(usage()),
        _ => Err(usage()),
    }
}

fn usage() -> String {
    "usage:\n\
     \x20 aether-admin key public --seed <file>\n\
     \x20 aether-admin package build --manifest <file> --payload-dir <dir> --seed <file> --out <file>\n\
     \x20 aether-admin package verify --package <file> --trust <name>:<hex-key>\n\
     \x20 aether-admin update sign --image <file> --name <image-name> --version <x.y.z> --counter <n> --channel <developer|beta|stable> --publisher <name> --seed <file> --out <file>\n\
     \x20 aether-admin update verify --manifest <file> --image <file> --trust <name>:<hex-key>\n\
     \x20 aether-admin update policy --manifest <file> --image <file> --trust <name>:<hex-key> --installed <x.y.z> --counter <n> --channel <channel> [--allow-older]\n\
     \x20 aether-admin release build --image <file> --name <name> --version <x.y.z> --counter <n> --channel <channel> --publisher <name> --seed <file> --commit <sha> --out-dir <dir>\n\
     \x20 aether-admin release verify --dir <dir> --trust <name>:<hex-key>\n\
     Private keys are 64 hex characters in a file; keep them outside the repository."
        .to_string()
}

fn key_public(args: &[String]) -> Result<()> {
    let seed = read_seed(&flag(args, "--seed")?)?;
    let public = ed25519::public_key_from_seed(&seed);
    println!("public-key={}", hex(&public));
    Ok(())
}

fn package_build(args: &[String]) -> Result<()> {
    let manifest_path = flag(args, "--manifest")?;
    let seed = read_seed(&flag(args, "--seed")?)?;
    let output = flag(args, "--out")?;
    let manifest_bytes = fs::read(&manifest_path)
        .map_err(|error| format!("cannot read manifest {}: {error}", manifest_path.display()))?;
    let fields = ManifestFields::parse(&manifest_bytes).map_err(|error| format!("{error:?}"))?;

    let payload = read_payload_directory(&flag(args, "--payload-dir")?)?;

    let digest = sha256(&payload);
    let manifest = fields.build(digest).map_err(|error| format!("{error:?}"))?;
    let mut body = vec![0u8; MAX_MANIFEST_BYTES];
    let body_length = manifest
        .render(&mut body)
        .ok_or("canonical manifest is larger than the limit")?;
    body.truncate(body_length);
    let signature = ed25519::sign(&seed, &body);
    let mut package = vec![0u8; aether_packaging::MAX_PACKAGE_BYTES];
    let length = encode_frame(
        &PACKAGE_MAGIC,
        PACKAGE_FORMAT,
        &body,
        &payload,
        &signature,
        &mut package,
    )
    .ok_or("package is larger than the format limit")?;
    package.truncate(length);

    write_output(&output, &package)?;
    println!(
        "package={} id={} version={} payload-bytes={} sha256={}",
        output.display(),
        manifest.id,
        version_text(manifest.version),
        payload.len(),
        hex(&digest)
    );
    Ok(())
}

fn package_verify(args: &[String]) -> Result<()> {
    let package_path = flag(args, "--package")?;
    let (name, key) = trust_flag(args)?;
    let bytes = fs::read(&package_path)
        .map_err(|error| format!("cannot read {}: {error}", package_path.display()))?;
    let publisher = TrustedPublisher { name: &name, key };
    let store = TrustStore::new(std::slice::from_ref(&publisher));
    let verified =
        aether_packaging::verify_package(&bytes, &store).map_err(|error| format!("{error:?}"))?;
    println!(
        "verified id={} version={} publisher={} files={}",
        verified.manifest.id,
        version_text(verified.manifest.version),
        verified.manifest.publisher,
        verified.payload.len()
    );
    Ok(())
}

fn update_sign(args: &[String]) -> Result<()> {
    let image_path = flag(args, "--image")?;
    let name = flag(args, "--name")?;
    let version = flag(args, "--version")?;
    let counter = flag(args, "--counter")?;
    let channel = flag(args, "--channel")?;
    let publisher = flag(args, "--publisher")?;
    let seed = read_seed(&flag(args, "--seed")?)?;
    let output = flag(args, "--out")?;

    let image_name = name.to_str().ok_or("image name is not valid UTF-8")?;
    if !is_safe_relative_path(image_name) {
        return Err(format!(
            "image name {image_name:?} is not a safe relative path"
        ));
    }
    let version_arg = version.to_str().ok_or("version is not UTF-8")?;
    let version = Version::parse(version_arg).map_err(|error| format!("{error:?}"))?;
    let counter_text = counter.to_str().ok_or("counter is not UTF-8")?;
    let counter: u64 = counter_text
        .parse()
        .map_err(|_| "counter must be a decimal u64".to_string())?;
    let channel = Channel::parse(channel.to_str().ok_or("channel is not UTF-8")?)
        .map_err(|error| format!("{error:?}"))?;
    let publisher = publisher.to_str().ok_or("publisher is not UTF-8")?;
    let image = fs::read(&image_path)
        .map_err(|error| format!("cannot read image {}: {error}", image_path.display()))?;

    let manifest = UpdateManifest {
        image: image_name,
        version,
        counter,
        channel,
        publisher,
        publisher_key: ed25519::public_key_from_seed(&seed),
        image_digest: sha256(&image),
    };
    let mut body = vec![0u8; aether_update::MAX_UPDATE_MANIFEST_BYTES];
    let body_length = manifest
        .render(&mut body)
        .ok_or("manifest is larger than the limit")?;
    body.truncate(body_length);
    // Self-check: the signed text must parse back to the same fields.
    UpdateManifest::parse(&body).map_err(|error| format!("{error:?}"))?;
    let signature = ed25519::sign(&seed, &body);
    let mut record = vec![0u8; 8192];
    let length = encode_frame(
        &UPDATE_MAGIC,
        UPDATE_FORMAT,
        &body,
        &[],
        &signature,
        &mut record,
    )
    .ok_or("update manifest is larger than the limit")?;
    record.truncate(length);
    write_output(&output, &record)?;
    println!(
        "manifest={} image={} version={} counter={} channel={} image-sha256={}",
        output.display(),
        image_name,
        version_text(version),
        counter,
        channel.name(),
        hex(&manifest.image_digest)
    );
    Ok(())
}

fn update_verify(args: &[String]) -> Result<()> {
    let (manifest, image) = load_update(args)?;
    let (name, key) = trust_flag(args)?;
    let publisher = TrustedPublisher { name: &name, key };
    let store = TrustStore::new(std::slice::from_ref(&publisher));
    let verified =
        verify_update(&manifest, &image, &store).map_err(|error| format!("{error:?}"))?;
    println!(
        "verified image={} version={} counter={} channel={}",
        verified.image,
        version_text(verified.version),
        verified.counter,
        verified.channel.name()
    );
    Ok(())
}

fn update_policy(args: &[String]) -> Result<()> {
    let (manifest, image) = load_update(args)?;
    let (name, key) = trust_flag(args)?;
    let installed = flag(args, "--installed")?;
    let installed = Version::parse(installed.to_str().ok_or("installed version is not UTF-8")?)
        .map_err(|error| format!("{error:?}"))?;
    let counter_text = flag(args, "--counter")?;
    let counter: u64 = counter_text
        .to_str()
        .ok_or("counter is not UTF-8")?
        .parse()
        .map_err(|_| "counter must be a decimal u64".to_string())?;
    let channel = Channel::parse(
        flag(args, "--channel")?
            .to_str()
            .ok_or("channel is not UTF-8")?,
    )
    .map_err(|error| format!("{error:?}"))?;

    let publisher = TrustedPublisher { name: &name, key };
    let store = TrustStore::new(std::slice::from_ref(&publisher));
    let verified =
        verify_update(&manifest, &image, &store).map_err(|error| format!("{error:?}"))?;
    let state = aether_update::InstalledState {
        version: installed,
        counter,
        channel,
    };
    let options = aether_update::PolicyOptions {
        allow_older_version: args.iter().any(|argument| argument == "--allow-older"),
    };
    match check_policy(&verified, state, options) {
        Ok(()) => {
            println!(
                "policy=accepted version={} counter={}",
                version_text(verified.version),
                verified.counter
            );
            Ok(())
        }
        Err(error) => Err(format!("policy rejected: {error:?}")),
    }
}

fn release_build(args: &[String]) -> Result<()> {
    let image_path = flag(args, "--image")?;
    let name = flag(args, "--name")?;
    let version = flag(args, "--version")?;
    let counter = flag(args, "--counter")?;
    let channel = flag(args, "--channel")?;
    let publisher = flag(args, "--publisher")?;
    let commit = flag(args, "--commit")?;
    let out_dir = flag(args, "--out-dir")?;
    let seed = read_seed(&flag(args, "--seed")?)?;

    let image_name = name.to_str().ok_or("image name is not valid UTF-8")?;
    if !is_safe_relative_path(image_name) {
        return Err(format!(
            "image name {image_name:?} is not a safe relative path"
        ));
    }
    let version = Version::parse(version.to_str().ok_or("version is not UTF-8")?)
        .map_err(|error| format!("{error:?}"))?;
    let counter: u64 = counter
        .to_str()
        .ok_or("counter is not UTF-8")?
        .parse()
        .map_err(|_| "counter must be a decimal u64".to_string())?;
    let channel = Channel::parse(channel.to_str().ok_or("channel is not UTF-8")?)
        .map_err(|error| format!("{error:?}"))?;
    let publisher = publisher.to_str().ok_or("publisher is not UTF-8")?;
    let commit = commit.to_str().ok_or("commit is not UTF-8")?;
    if publisher.is_empty() || publisher.len() > aether_packaging::manifest::MAX_PUBLISHER {
        return Err("publisher name is empty or too long".into());
    }

    let image = fs::read(&image_path)
        .map_err(|error| format!("cannot read image {}: {error}", image_path.display()))?;
    fs::create_dir_all(&out_dir)
        .map_err(|error| format!("cannot create {}: {error}", out_dir.display()))?;
    let image_out = out_dir.join(image_name);
    fs::write(&image_out, &image)
        .map_err(|error| format!("cannot write {}: {error}", image_out.display()))?;

    let digest = sha256(&image);
    let manifest = UpdateManifest {
        image: image_name,
        version,
        counter,
        channel,
        publisher,
        publisher_key: ed25519::public_key_from_seed(&seed),
        image_digest: digest,
    };
    let mut body = vec![0u8; aether_update::MAX_UPDATE_MANIFEST_BYTES];
    let body_length = manifest
        .render(&mut body)
        .ok_or("manifest is larger than the limit")?;
    body.truncate(body_length);
    UpdateManifest::parse(&body).map_err(|error| format!("{error:?}"))?;
    let signature = ed25519::sign(&seed, &body);
    let mut record = vec![0u8; 8192];
    let length = encode_frame(
        &UPDATE_MAGIC,
        UPDATE_FORMAT,
        &body,
        &[],
        &signature,
        &mut record,
    )
    .ok_or("update manifest is larger than the limit")?;
    record.truncate(length);
    let manifest_out = out_dir.join(format!("{image_name}.manifest"));
    fs::write(&manifest_out, &record)
        .map_err(|error| format!("cannot write {}: {error}", manifest_out.display()))?;

    let checksums = format!(
        "{}  {}\n{}  {}.manifest\n",
        hex(&digest),
        image_name,
        hex(&sha256(&record)),
        image_name
    );
    fs::write(out_dir.join("SHA256SUMS"), checksums)
        .map_err(|error| format!("cannot write SHA256SUMS: {error}"))?;

    let info = format!(
        "version={}.{}.{}\n\
         counter={counter}\n\
         channel={}\n\
         publisher={publisher}\n\
         commit={commit}\n\
         image={image_name}\n\
         image.sha256={}\n\
         image.bytes={}\n",
        version.major,
        version.minor,
        version.patch,
        channel.name(),
        hex(&digest),
        image.len()
    );
    fs::write(out_dir.join("release-info.txt"), info)
        .map_err(|error| format!("cannot write release-info.txt: {error}"))?;

    println!(
        "release dir={} image={} version={} counter={} sha256={}",
        out_dir.display(),
        image_name,
        version_text(version),
        counter,
        hex(&digest)
    );
    Ok(())
}

fn release_verify(args: &[String]) -> Result<()> {
    let directory = flag(args, "--dir")?;
    let (name, key) = trust_flag(args)?;
    let publisher = TrustedPublisher { name: &name, key };
    let store = TrustStore::new(std::slice::from_ref(&publisher));

    let info = fs::read_to_string(directory.join("release-info.txt"))
        .map_err(|error| format!("cannot read release-info.txt: {error}"))?;
    let mut image_name = None;
    let mut declared_digest = None;
    for line in info.lines() {
        if let Some(value) = line.strip_prefix("image=") {
            image_name = Some(value.to_string());
        }
        if let Some(value) = line.strip_prefix("image.sha256=") {
            let mut digest = [0u8; 32];
            if from_hex(value.as_bytes(), &mut digest) != Some(32) {
                return Err("release-info.txt has an invalid image digest".into());
            }
            declared_digest = Some(digest);
        }
    }
    let image_name = image_name.ok_or("release-info.txt has no image name")?;
    let declared_digest = declared_digest.ok_or("release-info.txt has no image digest")?;

    let checksums = fs::read_to_string(directory.join("SHA256SUMS"))
        .map_err(|error| format!("cannot read SHA256SUMS: {error}"))?;
    let mut listed = None;
    for line in checksums.lines() {
        let Some((digest_text, file_name)) = line.split_once("  ") else {
            return Err(format!("malformed SHA256SUMS line {line:?}"));
        };
        if file_name == image_name {
            let mut digest = [0u8; 32];
            if from_hex(digest_text.as_bytes(), &mut digest) != Some(32) {
                return Err("SHA256SUMS has an invalid digest".into());
            }
            listed = Some(digest);
        }
    }
    if listed != Some(declared_digest) {
        return Err("SHA256SUMS and release-info.txt disagree about the image".into());
    }

    let image = fs::read(directory.join(&image_name))
        .map_err(|error| format!("cannot read {image_name}: {error}"))?;
    let manifest = fs::read(directory.join(format!("{image_name}.manifest")))
        .map_err(|error| format!("cannot read the release manifest: {error}"))?;
    let verified =
        verify_update(&manifest, &image, &store).map_err(|error| format!("{error:?}"))?;
    if verified.image != image_name {
        return Err("manifest names a different image".into());
    }
    println!(
        "release verified image={} version={} counter={} channel={} publisher={}",
        verified.image,
        version_text(verified.version),
        verified.counter,
        verified.channel.name(),
        verified.publisher
    );
    Ok(())
}

fn load_update(args: &[String]) -> Result<(Vec<u8>, Vec<u8>)> {
    let manifest_path = flag(args, "--manifest")?;
    let image_path = flag(args, "--image")?;
    let manifest = fs::read(&manifest_path)
        .map_err(|error| format!("cannot read {}: {error}", manifest_path.display()))?;
    let image = fs::read(&image_path)
        .map_err(|error| format!("cannot read {}: {error}", image_path.display()))?;
    Ok((manifest, image))
}

fn read_payload_directory(directory: &Path) -> Result<Vec<u8>> {
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let entries = fs::read_dir(directory)
        .map_err(|error| format!("cannot read {}: {error}", directory.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("cannot read directory entry: {error}"))?;
        let metadata = entry
            .metadata()
            .map_err(|error| format!("cannot stat {}: {error}", entry.path().display()))?;
        if !metadata.is_file() {
            return Err(format!(
                "{} is not a regular file; package payloads are flat",
                entry.path().display()
            ));
        }
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "payload file name is not UTF-8".to_string())?;
        let data = fs::read(entry.path())
            .map_err(|error| format!("cannot read {}: {error}", entry.path().display()))?;
        files.push((name, data));
    }
    files.sort_by(|left, right| left.0.cmp(&right.0));
    let references: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(name, data)| (name.as_str(), data.as_slice()))
        .collect();
    let mut payload = vec![0u8; aether_packaging::MAX_PAYLOAD_BYTES];
    let length = encode_payload(&references, &mut payload)
        .ok_or("payload violates the file-count, name, or size limits")?;
    payload.truncate(length);
    Ok(payload)
}

fn read_seed(path: &Path) -> Result<[u8; 32]> {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("cannot read key {}: {error}", path.display()))?;
    let trimmed = text.trim();
    let mut seed = [0u8; 32];
    if from_hex(trimmed.as_bytes(), &mut seed) != Some(32) {
        return Err(format!(
            "{} is not a 32-byte (64 hex character) Ed25519 seed",
            path.display()
        ));
    }
    Ok(seed)
}

fn flag(args: &[String], name: &str) -> Result<PathBuf> {
    flag_optional(args, name).ok_or_else(|| format!("missing {name}\n{}", usage()))
}

fn flag_optional(args: &[String], name: &str) -> Option<PathBuf> {
    args.iter()
        .position(|argument| argument == name)
        .and_then(|index| args.get(index + 1))
        .map(PathBuf::from)
}

fn trust_flag(args: &[String]) -> Result<(String, [u8; 32])> {
    let value = flag(args, "--trust")?;
    let text = value.to_str().ok_or("--trust is not valid UTF-8")?;
    let (name, key) = text
        .split_once(':')
        .ok_or("--trust must be <name>:<public-key-hex>")?;
    let name = name.trim();
    if name.is_empty() {
        return Err("--trust publisher name is empty".into());
    }
    let mut bytes = [0u8; 32];
    if from_hex(key.trim().as_bytes(), &mut bytes) != Some(32) {
        return Err("--trust key must be 64 hex characters".into());
    }
    Ok((name.to_string(), bytes))
}

fn write_output(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    fs::write(path, bytes).map_err(|error| format!("cannot write {}: {error}", path.display()))
}

fn hex(bytes: &[u8]) -> String {
    let mut out = vec![0u8; bytes.len() * 2];
    let length = to_hex(bytes, &mut out).expect("output buffer is large enough");
    String::from_utf8(out[..length].to_vec()).expect("hex is ASCII")
}

fn version_text(version: Version) -> String {
    format!("{}.{}.{}", version.major, version.minor, version.patch)
}
