// SPDX-License-Identifier: Apache-2.0
//! Host tests for the virtual filesystem.

use super::*;

fn filesystem() -> AetherFs<RamDisk> {
    let mut fs = AetherFs::new(RamDisk::new());
    fs.format().expect("format");
    fs.mkdir("/", "apps").expect("apps");
    fs.mkdir("/", "apps/demo").expect("demo");
    fs.mkdir("/", "apps/demo/private").expect("demo private");
    fs.mkdir("/", "apps/other").expect("other");
    fs.mkdir("/", "apps/other/private").expect("other private");
    fs.mkdir("/", "system").expect("system");
    fs
}

#[test]
fn formats_and_persists_directories_and_files() {
    let mut fs = filesystem();
    fs.write_file("/", "/apps/demo/private/notes.txt", b"hello")
        .expect("write");
    let mut buffer = [0u8; 32];
    let length = fs
        .read_file("/apps/demo", "private/notes.txt", &mut buffer)
        .expect("read");
    assert_eq!(&buffer[..length], b"hello");
    assert_eq!(fs.stat("/apps/demo", "private/notes.txt").expect("stat"), 5);

    let mut list = [0u8; 128];
    let length = fs.list("/apps/demo", "private", &mut list).expect("list");
    assert_eq!(&list[..length], b"notes.txt\tf\t5\n");
}

#[test]
fn scoped_resolution_rejects_traversal_and_sibling_access() {
    let mut fs = filesystem();
    fs.write_file("/", "/apps/other/private/secret.txt", b"secret")
        .expect("secret");
    let mut buffer = [0u8; 32];
    assert_eq!(
        fs.read_file("/apps/demo", "../other/private/secret.txt", &mut buffer),
        Err(FsError::Escape)
    );
    assert_eq!(
        fs.read_file("/apps/demo", "../../system/policy.txt", &mut buffer),
        Err(FsError::Escape)
    );
    assert_eq!(
        fs.read_file("/apps/demo", "..%2fother", &mut buffer),
        Err(FsError::NotFound)
    );
    // A leading slash is still scoped to the caller root, not the filesystem
    // root. It cannot be used to name a sibling.
    assert_eq!(
        fs.read_file("/apps/demo", "/../other/private/secret.txt", &mut buffer),
        Err(FsError::Escape)
    );
}

#[test]
fn open_handles_are_generation_checked_and_rights_checked() {
    let mut fs = filesystem();
    fs.write_file("/", "/apps/demo/private/notes.txt", b"abc")
        .expect("write");
    let read = fs
        .open("/apps/demo", "private/notes.txt", Access::READ)
        .expect("open");
    let mut buffer = [0u8; 8];
    assert_eq!(fs.read(read, &mut buffer), Ok(3));
    assert_eq!(
        fs.write(read, b"x"),
        Err(FsError::Access),
        "read-only handle must reject writes"
    );
    fs.close(read).expect("close");
    assert_eq!(fs.read(read, &mut buffer), Err(FsError::BadHandle));

    let read_again = fs
        .open("/apps/demo", "private/notes.txt", Access::READ)
        .expect("reopen");
    assert_eq!(read_again.index, read.index);
    assert_ne!(read_again.generation, read.generation);
    assert_eq!(fs.read(read, &mut buffer), Err(FsError::BadHandle));
}

#[test]
fn write_extends_and_replaces_content() {
    let mut fs = filesystem();
    let id = fs
        .open(
            "/apps/demo",
            "private/notes.txt",
            Access::READ | Access::WRITE | Access::CREATE,
        )
        .expect("create");
    assert_eq!(fs.write(id, b"hello "), Ok(6));
    assert_eq!(fs.write(id, b"world"), Ok(5));
    fs.close(id).expect("close");

    let mut buffer = [0u8; 32];
    let length = fs
        .read_file("/apps/demo", "private/notes.txt", &mut buffer)
        .expect("read");
    assert_eq!(&buffer[..length], b"hello world");
}

#[test]
fn file_size_is_bounded_and_disk_exhaustion_is_reported() {
    let mut fs = filesystem();
    let oversized = [0u8; MAX_FILE_BYTES + 1];
    assert_eq!(
        fs.write_file("/", "/apps/demo/private/big", &oversized),
        Err(FsError::FileTooLarge)
    );

    // Exhaust the block bitmap directly so the test does not depend on how
    // many inodes remain. A create may still leave an empty inode, but it must
    // report NoSpace instead of writing partial data.
    for block in DATA_START_BLOCK..BLOCK_COUNT as u16 {
        fs.set_block_used(block, true).expect("mark block used");
    }
    assert_eq!(
        fs.write_file("/", "/system/too-many", &[0u8; 16]),
        Err(FsError::NoSpace)
    );
}

#[test]
fn debug_directory_layout() {
    let mut fs = filesystem();
    let resolved = resolve("/apps/demo", "private/notes.txt").expect("resolve");
    std::println!(
        "resolved={} components={:?}",
        resolved.as_str(),
        resolved.components().collect::<Vec<_>>()
    );
    match fs.lookup(&resolved) {
        Ok(inode) => std::println!(
            "lookup inode={inode} mode={}",
            fs.read_inode(inode).expect("inode").mode
        ),
        Err(error) => std::println!("lookup error={error:?}"),
    }
    let (parent_path, name) = resolved.parent_and_name().expect("parent");
    std::println!("parent={} name={name}", parent_path.as_str());
    let parent = fs.lookup(&parent_path).expect("parent inode");
    std::println!(
        "parent inode={parent} dir={}",
        fs.read_inode(parent).expect("p").is_dir()
    );
    if fs
        .dir_find(parent, name.as_bytes())
        .expect("find")
        .is_none()
    {
        let inode = fs.allocate_inode().expect("alloc inode");
        std::println!("new inode={inode}");
        fs.write_inode(
            inode,
            Inode {
                mode: MODE_FILE,
                ..Inode::EMPTY
            },
        )
        .expect("write inode");
        fs.dir_add(parent, name.as_bytes(), inode, false)
            .expect("dir add");
        let read = fs.read_inode(inode).expect("read new");
        std::println!("new inode mode={} is_file={}", read.mode, read.is_file());
        let parent_inode = fs.read_inode(parent).expect("parent2");
        let entries = fs.dir_entries(parent_inode).expect("entries");
        for entry in entries.into_iter().flatten() {
            std::println!(
                "entry {} inode={} dir={}",
                entry.name_len,
                entry.inode,
                entry.is_dir
            );
        }
    }
    for path in ["/", "/apps", "/apps/demo", "/apps/demo/private"] {
        let mut list = [0u8; 256];
        match fs.list("/", path, &mut list) {
            Ok(length) => std::println!("{path}: {:?}", core::str::from_utf8(&list[..length])),
            Err(error) => std::println!("{path}: error {error:?}"),
        }
    }
}

#[test]
fn duplicate_names_are_rejected() {
    let mut fs = filesystem();
    assert_eq!(fs.mkdir("/", "apps/demo/private"), Err(FsError::Exists));
    fs.write_file("/", "apps/demo/file", b"x").expect("file");
    fs.write_file("/", "apps/demo/file", b"y").expect("replace");
    let mut buffer = [0u8; 8];
    let length = fs
        .read_file("/", "apps/demo/file", &mut buffer)
        .expect("read");
    assert_eq!(&buffer[..length], b"y");
}

#[test]
fn unformatted_or_wrong_version_devices_are_rejected() {
    let mut empty = AetherFs::new(RamDisk::new());
    let mut buffer = [0u8; 8];
    assert_eq!(
        empty.read_file("/", "missing", &mut buffer),
        Err(FsError::NotFormatted)
    );

    let mut fs = filesystem();
    fs.device.write_block(0, &[0u8; BLOCK_SIZE]).expect("wipe");
    assert_eq!(
        fs.read_file("/", "missing", &mut buffer),
        Err(FsError::NotFormatted)
    );

    let mut fs = filesystem();
    let mut superblock = [0u8; BLOCK_SIZE];
    fs.device
        .read_block(0, &mut superblock)
        .expect("read superblock");
    superblock[4..6].copy_from_slice(&(VERSION + 1).to_le_bytes());
    fs.device
        .write_block(0, &superblock)
        .expect("write superblock");
    assert_eq!(
        fs.read_file("/", "missing", &mut buffer),
        Err(FsError::Corrupt)
    );
}

#[test]
fn open_files_cannot_be_unlinked_until_closed() {
    let mut fs = filesystem();
    fs.write_file("/", "/apps/demo/private/held.txt", b"held")
        .expect("write");
    let handle = fs
        .open("/apps/demo", "private/held.txt", Access::READ)
        .expect("open");
    assert_eq!(
        fs.unlink("/apps/demo", "private/held.txt"),
        Err(FsError::Access)
    );
    fs.close(handle).expect("close");
    fs.unlink("/apps/demo", "private/held.txt")
        .expect("unlink after close");
}

#[test]
fn failed_replacement_preserves_the_previous_file() {
    let mut fs = filesystem();
    fs.write_file("/", "/apps/demo/private/keep.txt", b"keep")
        .expect("original");

    // Mark every data block used. The old implementation freed the target's
    // block before allocating the replacement, so it reused that block and
    // then failed halfway through, corrupting the file.
    for block in DATA_START_BLOCK..BLOCK_COUNT as u16 {
        fs.set_block_used(block, true).expect("mark block used");
    }

    assert_eq!(
        fs.write_file("/", "/apps/demo/private/keep.txt", &[0u8; MAX_FILE_BYTES]),
        Err(FsError::NoSpace)
    );

    let mut buffer = [0u8; 16];
    let length = fs
        .read_file("/apps/demo", "private/keep.txt", &mut buffer)
        .expect("old content still readable");
    assert_eq!(&buffer[..length], b"keep");
}

#[test]
fn corrupt_inode_metadata_is_rejected_without_panicking() {
    let mut fs = filesystem();

    // A size larger than the maximum file forces a block-table overrun unless
    // the inode is validated first.
    fs.write_inode(
        5,
        Inode {
            mode: MODE_FILE,
            size: (MAX_FILE_BYTES + 1) as u16,
            ..Inode::EMPTY
        },
    )
    .expect("write raw inode");
    assert!(matches!(fs.read_inode(5), Err(FsError::Corrupt)));

    // A block number outside the device must not reach the block layer.
    fs.write_inode(
        6,
        Inode {
            mode: MODE_FILE,
            size: BLOCK_SIZE as u16,
            blocks: [BLOCK_COUNT as u16, 0, 0, 0, 0, 0, 0, 0],
        },
    )
    .expect("write raw inode");
    assert!(matches!(fs.read_inode(6), Err(FsError::Corrupt)));

    // A directory size that is not a whole number of entries is corrupt.
    fs.write_inode(
        7,
        Inode {
            mode: MODE_DIR,
            size: 1,
            blocks: [DATA_START_BLOCK, 0, 0, 0, 0, 0, 0, 0],
        },
    )
    .expect("write raw inode");
    assert!(matches!(fs.read_inode(7), Err(FsError::Corrupt)));
}
