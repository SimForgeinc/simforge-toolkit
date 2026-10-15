use super::*;
use std::io::Cursor;
use std::process::Command;
fn archive(root: &Path, path: &str, kind: tar::EntryType, target: Option<&str>) -> PathBuf {
    let file = root.join("pack.tar.gz");
    let gzip = flate2::write::GzEncoder::new(
        fs::File::create(&file).unwrap(),
        flate2::Compression::default(),
    );
    let mut builder = tar::Builder::new(gzip);
    let mut header = tar::Header::new_ustar();
    header.as_mut_bytes()[..path.len()].copy_from_slice(path.as_bytes());
    header.set_entry_type(kind);
    header.set_size(0);
    header.set_mode(0o644);
    if let Some(target) = target {
        header.set_link_name(target).unwrap();
    }
    header.set_cksum();
    builder.append(&header, Cursor::new([])).unwrap();
    builder.into_inner().unwrap().finish().unwrap();
    file
}

#[test]
fn extraction_refuses_parent_escape_and_symlink_members() {
    for (path, kind, target) in [
        ("../escaped", tar::EntryType::Regular, None),
        ("bin/linked", tar::EntryType::Symlink, Some("/outside")),
        ("bin/global", tar::EntryType::new(b'g'), None),
        ("bin/long-name", tar::EntryType::new(b'L'), None),
        ("bin/long-link", tar::EntryType::new(b'K'), None),
    ] {
        let root = tempfile::tempdir().unwrap();
        let stage = root.path().join("stage");
        fs::create_dir(&stage).unwrap();
        let file = archive(root.path(), path, kind, target);
        assert_eq!(
            extract(&file, &stage, "runtime", "simforge-runtime")
                .unwrap_err()
                .code,
            "invalid_distribution_archive"
        );
        assert!(!root.path().join("escaped").exists());
        assert!(!stage.join("bin/linked").exists());
    }
}
#[test]
fn local_cli_extracts_archive_built_by_release_packer() {
    let root = tempfile::tempdir().unwrap();
    let output = root.path().join("packed");
    let target = root.path().join("cargo-target");
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap();
    let status = Command::new("node")
        .current_dir(repo)
        .args([
            "scripts/release/cloud-cli-artifact.mjs",
            "pack",
            "--root",
            repo.to_str().unwrap(),
            "--local",
            "--out-dir",
            output.to_str().unwrap(),
            "--target-dir",
            target.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(status.success(), "release packer exited with {status}");

    let stage = root.path().join("stage");
    fs::create_dir(&stage).unwrap();
    let archive = output.join("simforge-local-x86_64-unknown-linux-gnu.tar.gz");
    extract(&archive, &stage, "cli", "simforge-local").unwrap();
    for member in ["simforge", "LICENSE", "NOTICE", "THIRD_PARTY_NOTICES.txt"] {
        assert!(
            stage.join("simforge-local").join(member).is_file(),
            "{member} missing"
        );
    }
}

#[test]
fn local_cli_rejects_unsafe_members_and_second_top_level_root() {
    for (path, kind, target) in [
        ("/absolute", tar::EntryType::Regular, None),
        ("../escaped", tar::EntryType::Regular, None),
        (
            "simforge-local/linked",
            tar::EntryType::Symlink,
            Some("/outside"),
        ),
        (
            "simforge-local/hard",
            tar::EntryType::Link,
            Some("simforge-local/simforge"),
        ),
        ("simforge-local/device", tar::EntryType::new(b'3'), None),
        ("other/file", tar::EntryType::Regular, None),
    ] {
        let root = tempfile::tempdir().unwrap();
        let stage = root.path().join("stage");
        fs::create_dir(&stage).unwrap();
        let file = archive(root.path(), path, kind, target);
        assert_eq!(
            extract(&file, &stage, "cli", "simforge-local")
                .unwrap_err()
                .code,
            "invalid_distribution_archive",
            "{path} should be refused",
        );
        assert!(!stage.join("simforge-local/linked").exists());
        assert!(!root.path().join("escaped").exists());
    }
}

#[test]
fn local_cli_digest_mismatch_preserves_existing_install() {
    let root = tempfile::tempdir().unwrap();
    let stage = root.path().join("stage");
    fs::create_dir_all(stage.join("simforge-local")).unwrap();
    fs::write(stage.join("simforge-local/simforge"), b"new").unwrap();
    let prefix = root.path().join("bin");
    fs::create_dir_all(&prefix).unwrap();
    fs::write(prefix.join("simforge"), b"old").unwrap();
    let declared = json!({"sha256": "0".repeat(64), "bytes": 3});
    assert_eq!(
        install_cli_binary(&stage, &prefix, &declared)
            .unwrap_err()
            .code,
        "runtime_digest_mismatch"
    );
    assert_eq!(fs::read(prefix.join("simforge")).unwrap(), b"old");
    assert!(!prefix
        .join(format!(".simforge-{}.next", std::process::id()))
        .exists());
}

#[test]
fn interrupted_local_cli_install_leaves_no_partial_binary() {
    let root = tempfile::tempdir().unwrap();
    let stage = root.path().join("stage");
    fs::create_dir_all(stage.join("simforge-local")).unwrap();
    fs::write(stage.join("simforge-local/simforge"), b"new").unwrap();
    let prefix = root.path().join("missing-parent/bin");
    let declared = json!({"sha256": format!("{:x}", Sha256::digest(b"new")), "bytes": 3});
    assert!(install_cli_binary(&stage, &prefix, &declared).is_ok());
    assert_eq!(fs::read(prefix.join("simforge")).unwrap(), b"new");
    assert!(!prefix
        .join(format!(".simforge-{}.next", std::process::id()))
        .exists());
}

#[test]
fn unentitled_local_cli_error_requests_staff_access() {
    let error = local_cli_install_error(CliError::new("insufficient_scope", "download refused"));
    assert_eq!(
        error.detail.unwrap()["request"],
        "simforge-local CLI distribution access"
    );
}

#[test]
fn a_failed_third_profile_install_restores_all_previous_profiles() {
    let root = tempfile::tempdir().unwrap();
    let mut moves = Vec::new();
    for (index, family) in ["native", "semantic", "web"].iter().enumerate() {
        let destination = root.path().join(family).join("map");
        fs::create_dir_all(&destination).unwrap();
        fs::write(destination.join("map.xodr"), format!("old-{family}")).unwrap();
        let source = root.path().join(format!("stage-{family}"));
        if index < 2 {
            fs::create_dir(&source).unwrap();
            fs::write(source.join("map.xodr"), format!("new-{family}")).unwrap();
        }
        moves.push((source, destination));
    }
    assert_eq!(replace_group(&moves).unwrap_err().code, "install_io_error");
    for (_, destination) in &moves {
        let family = destination
            .parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(
            fs::read_to_string(destination.join("map.xodr")).unwrap(),
            format!("old-{family}")
        );
    }
}

#[test]
fn a_map_member_cannot_be_replaced_by_rewriting_only_the_archive_checksums() {
    let root = tempfile::tempdir().unwrap();
    let map = b"<OpenDRIVE/>";
    let mut sums = String::new();
    let mut receipt_hashes = json!({});
    for (family, profile) in [
        (".corpus", "native"),
        ("dev-assets", "semantic"),
        ("map-bundles", "web"),
    ] {
        let directory = root.path().join(family).join("map");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("map.xodr"), map).unwrap();
        let member_sha = format!("{:x}", Sha256::digest(map));
        let receipt = json!({"schema":"simforge.map-installation.v1","name":"map","version":"v1","releaseDigest":"a".repeat(64),"canonicalDigest":"b".repeat(64),"webDigest":"c".repeat(64),"derivatives":{},"profile":profile,"members":{"map.xodr":{"sha256":member_sha,"bytes":map.len()}}});
        let mut bytes = serde_json::to_vec(&receipt).unwrap();
        bytes.push(b'\n');
        fs::write(directory.join(".map-release.json"), &bytes).unwrap();
        let receipt_sha = format!("{:x}", Sha256::digest(&bytes));
        receipt_hashes[profile] = json!(receipt_sha);
        sums.push_str(&format!(
            "{member_sha}  {family}/map/map.xodr\n{receipt_sha}  {family}/map/.map-release.json\n"
        ));
    }
    fs::write(root.path().join("SHA256SUMS"), &sums).unwrap();
    let manifest = json!({"schema":"simforge.map-pack/v1","name":"map","version":"v1","releaseDigest":"a".repeat(64),"closureDigest":"b".repeat(64),"webDigest":"c".repeat(64),"derivatives":{},"membership":{"receiptSha256":receipt_hashes},"install":{"layout":{"native":".corpus/map","semantic":"dev-assets/map","web":"map-bundles/map"},"receipts":{"native":".corpus/map/.map-release.json","semantic":"dev-assets/map/.map-release.json","web":"map-bundles/map/.map-release.json"}}});
    verify_map(root.path(), &manifest, "map", "v1").unwrap();
    let replacement = b"<OpenDRIVE>changed</OpenDRIVE>";
    fs::write(root.path().join(".corpus/map/map.xodr"), replacement).unwrap();
    let old = format!("{:x}  .corpus/map/map.xodr", Sha256::digest(map));
    let new = format!("{:x}  .corpus/map/map.xodr", Sha256::digest(replacement));
    fs::write(root.path().join("SHA256SUMS"), sums.replace(&old, &new)).unwrap();
    assert_eq!(
        verify_map(root.path(), &manifest, "map", "v1")
            .unwrap_err()
            .code,
        "map_member_digest_mismatch"
    );
}

fn pax_archive(root: &Path, records: &[(&str, &[u8])]) -> PathBuf {
    let file = root.join("pack.tar.gz");
    let gzip = flate2::write::GzEncoder::new(
        fs::File::create(&file).unwrap(),
        flate2::Compression::default(),
    );
    let mut builder = tar::Builder::new(gzip);
    builder
        .append_pax_extensions(records.iter().copied())
        .unwrap();
    let bytes = b"original web member";
    let mut header = tar::Header::new_ustar();
    header.set_path("map-bundles/map/payload").unwrap();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder.append(&header, Cursor::new(bytes)).unwrap();
    builder.into_inner().unwrap().finish().unwrap();
    file
}

#[test]
fn pax_preserves_long_original_web_member_names_and_rejects_unsafe_records() {
    let root = tempfile::tempdir().unwrap();
    let stage = root.path().join("stage");
    fs::create_dir(&stage).unwrap();
    let long_path = format!(
        "map-bundles/map/3d/variants/browser-pack-textures-256-uastc-{}.json",
        "a".repeat(64)
    );
    let file = pax_archive(root.path(), &[("path", long_path.as_bytes())]);
    extract(&file, &stage, "map", "map").unwrap();
    assert_eq!(
        fs::read(stage.join(&long_path)).unwrap(),
        b"original web member"
    );
    assert!(!stage.join("map-bundles/map/payload").exists());
    for records in [
        vec![("path", b"../escaped".as_slice())],
        vec![("path", long_path.as_bytes()), ("size", b"0".as_slice())],
    ] {
        let file = pax_archive(root.path(), &records);
        assert_eq!(
            extract(&file, &stage, "map", "map").unwrap_err().code,
            "invalid_distribution_archive"
        );
        assert!(!root.path().join("escaped").exists());
    }
}

#[test]
fn a_download_reports_each_tenth_and_at_least_every_thirty_seconds() {
    let start = std::time::Instant::now();
    let mut progress = Progress {
        kind: "map",
        name: "richmond-field-station",
        version: "v9",
        total: 1000,
        done: 0,
        tenth: 0,
        at: start,
    };
    // Chunks below a tenth are quiet until the tenth is crossed.
    assert!(!progress.advance(50, start));
    assert!(progress.advance(60, start));
    assert_eq!(progress.line()["percent"], 11);
    assert!(!progress.advance(10, start));
    // A slow link still gets a line every 30 s.
    assert!(progress.advance(1, start + PROGRESS_EVERY));
    assert!(!progress.advance(1, start + PROGRESS_EVERY));
    // The last byte is the 100% line.
    assert!(progress.advance(878, start + PROGRESS_EVERY));
    let line = progress.line();
    assert_eq!(line["event"], DOWNLOAD_PROGRESS_EVENT);
    assert_eq!(line["bytes"], json!({"done": 1000, "total": 1000}));
    assert_eq!(line["percent"], 100);
}
