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
fn rename_copy_move_and_delete_preserve_contents_and_reject_unsafe_targets() {
    let mut fs = filesystem();
    fs.write_file("/", "/apps/demo/private/notes.txt", b"hello")
        .expect("write");
    fs.copy_file("/apps/demo", "private/notes.txt", "private/copy.txt")
        .expect("copy");
    let mut buffer = [0u8; 32];
    let length = fs
        .read_file("/apps/demo", "private/copy.txt", &mut buffer)
        .expect("read copy");
    assert_eq!(&buffer[..length], b"hello");
    assert_eq!(
        fs.copy_file("/apps/demo", "private/notes.txt", "private/copy.txt"),
        Err(FsError::Exists)
    );

    fs.rename("/apps/demo", "private/copy.txt", "private/moved.txt")
        .expect("rename");
    assert_eq!(
        fs.stat("/apps/demo", "private/copy.txt"),
        Err(FsError::NotFound)
    );
    assert_eq!(fs.stat("/apps/demo", "private/moved.txt"), Ok(5));

    fs.mkdir("/apps/demo", "private/nested").expect("nested");
    fs.rename("/apps/demo", "private/nested", "nested")
        .expect("move dir");
    assert!(fs.list("/apps/demo", "nested", &mut buffer).is_ok());

    // A directory cannot be moved inside itself, and a non-empty directory
    // cannot be removed. Both operations leave the tree intact.
    assert_eq!(
        fs.rename("/apps/demo", "nested", "nested/inside"),
        Err(FsError::Access)
    );
    fs.write_file("/apps/demo", "nested/file", b"x")
        .expect("nested file");
    assert!(fs.remove("/apps/demo", "nested").is_err());
    fs.remove("/apps/demo", "nested/file").expect("remove file");
    fs.remove("/apps/demo", "nested").expect("remove empty dir");
    assert_eq!(fs.stat("/apps/demo", "nested"), Err(FsError::NotFound));
}

#[test]
fn open_files_block_deletion_until_closed() {
    let mut fs = filesystem();
    fs.write_file("/", "/apps/demo/private/notes.txt", b"abc")
        .expect("write");
    let id = fs
        .open("/apps/demo", "private/notes.txt", Access::READ)
        .expect("open");
    assert_eq!(
        fs.remove("/apps/demo", "private/notes.txt"),
        Err(FsError::Access)
    );
    fs.close(id).expect("close");
    fs.remove("/apps/demo", "private/notes.txt")
        .expect("delete");
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

    // A data pointer into the superblock, inode table, or other reserved
    // metadata blocks must be rejected rather than exposing metadata as file
    // contents or allowing a later operation to treat it as data.
    for block in 0..DATA_START_BLOCK {
        fs.write_inode(
            8,
            Inode {
                mode: MODE_FILE,
                size: BLOCK_SIZE as u16,
                blocks: [block, 0, 0, 0, 0, 0, 0, 0],
            },
        )
        .expect("write raw inode");
        assert!(
            matches!(fs.read_inode(8), Err(FsError::Corrupt)),
            "metadata block {block} was accepted as file data"
        );
    }

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

struct StressRng(u64);

impl StressRng {
    fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    fn next(&mut self) -> u64 {
        // xorshift64*, deterministic and dependency-free.
        let mut value = self.0;
        value ^= value >> 12;
        value ^= value << 25;
        value ^= value >> 27;
        self.0 = value;
        value.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, limit: usize) -> usize {
        (self.next() as usize) % limit
    }
}

#[test]
fn control_characters_cannot_inject_listing_records() {
    let mut fs = filesystem();
    for name in [
        "line\nbreak",
        "tab\tname",
        "nul\0name",
        "carriage\rreturn",
        "escape\x1bname",
    ] {
        assert_eq!(
            fs.write_file("/", name, b"x"),
            Err(FsError::NameTooLong),
            "{name:?}"
        );
        assert_eq!(fs.mkdir("/", name), Err(FsError::NameTooLong), "{name:?}");
    }
    // A long-but-valid name still works.
    fs.write_file("/", "abcdefghijklmnop", b"x")
        .expect("maximum name length");
}

#[test]
fn corrupt_directory_entries_fail_closed() {
    let mut fs = filesystem();
    fs.mkdir("/", "dir").expect("dir");
    fs.write_file("/", "dir/file", b"x").expect("file");
    let resolved = resolve("/", "dir").expect("resolve dir");
    let inode_number = fs.lookup(&resolved).expect("dir inode");
    let inode = fs.read_inode(inode_number).expect("inode");
    let block = inode.blocks[0];
    let mut data = [0u8; BLOCK_SIZE];
    fs.device.read_block(block, &mut data).expect("read");

    // A newline in a stored name would forge a second listing line.
    let mut injected = data;
    injected[0] = b'\n';
    fs.device.write_block(block, &injected).expect("write");
    let mut out = [0u8; 128];
    assert_eq!(fs.list("/", "dir", &mut out), Err(FsError::Corrupt));

    // A zero inode is not a valid directory entry.
    let mut zero = data;
    zero[16] = 0;
    zero[17] = 0;
    fs.device.write_block(block, &zero).expect("write");
    assert_eq!(fs.list("/", "dir", &mut out), Err(FsError::Corrupt));

    // An unknown kind byte is not silently treated as a regular file.
    let mut kind = data;
    kind[18] = 9;
    fs.device.write_block(block, &kind).expect("write");
    assert_eq!(fs.list("/", "dir", &mut out), Err(FsError::Corrupt));

    fs.device.write_block(block, &data).expect("restore");
    assert!(fs.list("/", "dir", &mut out).is_ok());
}

#[test]
fn rename_to_the_same_missing_path_reports_not_found() {
    let mut fs = filesystem();
    assert_eq!(
        fs.rename("/apps/demo", "private/missing", "private/missing"),
        Err(FsError::NotFound)
    );
    fs.write_file("/apps/demo", "private/present", b"x")
        .expect("write");
    fs.rename("/apps/demo", "private/present", "private/present")
        .expect("existing self rename is a no-op");
}

#[test]
fn randomized_file_operations_never_panic_and_remain_consistent() {
    let mut fs = filesystem();
    let mut rng = StressRng::new(0xa37e_5eed);
    let dirs = ["private", "private/d0", "private/d1", "private/d2"];
    for dir in dirs {
        let _ = fs.mkdir("/apps/demo", dir);
    }

    for _ in 0..4000 {
        let dir = dirs[rng.below(dirs.len())];
        let name = std::format!("f{}", rng.below(10));
        let path = std::format!("{dir}/{name}");
        match rng.below(8) {
            0 | 1 => {
                let length = rng.below(MAX_FILE_BYTES + 1);
                let mut data = std::vec![0u8; length];
                for (index, byte) in data.iter_mut().enumerate() {
                    *byte = (index.wrapping_add(length)) as u8;
                }
                let _ = fs.write_file("/apps/demo", &path, &data);
            }
            2 => {
                let _ = fs.copy_file("/apps/demo", &path, &std::format!("{dir}/copy"));
            }
            3 => {
                let _ = fs.rename(
                    "/apps/demo",
                    &path,
                    &std::format!("{dir}/moved{}", rng.below(10)),
                );
            }
            4 => {
                let _ = fs.remove("/apps/demo", &path);
            }
            5 => {
                let mut out = [0u8; 256];
                let _ = fs.list("/apps/demo", dir, &mut out);
            }
            6 => {
                let mut buffer = [0u8; MAX_FILE_BYTES];
                if let Ok(length) = fs.read_file("/apps/demo", &path, &mut buffer) {
                    assert!(length <= MAX_FILE_BYTES);
                }
            }
            _ => {
                if let Ok(id) = fs.open("/apps/demo", &path, Access::READ) {
                    let mut buffer = [0u8; 64];
                    let _ = fs.read(id, &mut buffer);
                    let _ = fs.close(id);
                }
            }
        }
    }

    // The churn may legitimately exhaust the bounded inode or block table.
    // Removing the known names must release space and leave the service usable.
    for dir in dirs {
        for index in 0..10 {
            let _ = fs.remove("/apps/demo", &std::format!("{dir}/f{index}"));
            let _ = fs.remove("/apps/demo", &std::format!("{dir}/moved{index}"));
        }
        let _ = fs.remove("/apps/demo", &std::format!("{dir}/copy"));
    }
    fs.write_file("/apps/demo", "private/final", b"final")
        .expect("final write");
    let mut buffer = [0u8; 16];
    let length = fs
        .read_file("/apps/demo", "private/final", &mut buffer)
        .expect("final read");
    assert_eq!(&buffer[..length], b"final");
}

#[test]
fn corrupted_images_never_panic_metadata_paths() {
    for seed in 1..=160u64 {
        let mut fs = AetherFs::new(RamDisk::new());
        fs.format().expect("format");
        fs.mkdir("/", "home").expect("home");
        fs.mkdir("/", "apps").expect("apps");
        fs.write_file("/", "/home/welcome.txt", b"welcome\n")
            .expect("welcome");

        let mut rng = StressRng::new(seed);
        // Flip a handful of bytes in the superblock, inode table, bitmap, and
        // data area. Every operation must return an error or a safe result.
        for _ in 0..(1 + rng.below(8)) {
            let block = rng.below(BLOCK_COUNT) as u16;
            let offset = rng.below(BLOCK_SIZE);
            let mut data = [0u8; BLOCK_SIZE];
            fs.device.read_block(block, &mut data).expect("read");
            data[offset] ^= 1 << rng.below(8);
            fs.device.write_block(block, &data).expect("write");
        }

        let mut out = [0u8; 512];
        let _ = fs.list("/", "/", &mut out);
        let _ = fs.list("/", "home", &mut out);
        let _ = fs.read_file("/", "home/welcome.txt", &mut out);
        let _ = fs.stat("/", "home/welcome.txt");
        let _ = fs.mkdir("/", "newdir");
        let _ = fs.write_file("/", "home/new.txt", b"data");
        let _ = fs.rename("/", "home/new.txt", "home/other.txt");
        let _ = fs.remove("/", "home/new.txt");
        if let Ok(id) = fs.open("/", "home/welcome.txt", Access::READ) {
            let mut small = [0u8; 32];
            let _ = fs.read(id, &mut small);
            let _ = fs.close(id);
        }
    }
}
