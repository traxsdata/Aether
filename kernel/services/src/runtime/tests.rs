// SPDX-License-Identifier: Apache-2.0
//! Host tests for runtime profile policy.

use super::*;
use aether_packaging::manifest::{Limits, Permissions};

fn profile(id: &'static str) -> RuntimeProfile<'static> {
    RuntimeProfile {
        id,
        prefix: match id {
            "org.aether.demo" => "/apps/org.aether.demo",
            "org.aether.other" => "/apps/org.aether.other",
            _ => "/apps/unknown",
        },
        permissions: Permissions::parse_list("file.read,file.write").unwrap(),
        limits: Limits {
            memory_kib: 256,
            open_files: 4,
            tasks: 1,
        },
        shared_prefix: false,
    }
}

#[test]
fn prefixes_are_derived_from_identity() {
    let mut buffer = [0u8; 64];
    let prefix = derive_prefix("org.aether.demo", &mut buffer).unwrap();
    assert_eq!(prefix, "/apps/org.aether.demo");
    assert!(is_canonical_prefix("org.aether.demo", prefix));

    let mut small = [0u8; 4];
    assert!(derive_prefix("org.aether.demo", &mut small).is_none());
    assert!(derive_prefix("Not.A.Valid.Id", &mut buffer).is_none());

    assert!(
        isolated(
            "org.aether.demo",
            "/apps/org.aether.other",
            Permissions::parse_list("file.read").unwrap(),
            profile("org.aether.demo").limits
        )
        .is_err()
    );
}

#[test]
fn profiles_carry_the_declared_permissions_and_limits() {
    let profile = profile("org.aether.demo");
    assert!(profile.allows(Permissions::FILE_READ));
    assert!(!profile.allows(Permissions::PORTAL_READ));
    assert!(profile.allows_open_files(4));
    assert!(!profile.allows_open_files(5));
    assert!(profile.allows_memory(256));
    assert!(!profile.allows_memory(257));
    assert!(profile.allows_tasks(1));
    assert!(!profile.allows_tasks(2));
}

#[test]
fn relative_paths_cannot_escape_the_prefix() {
    let profile = profile("org.aether.demo");
    assert_eq!(profile.check_relative_path("demo.bin"), Ok(()));
    assert_eq!(profile.check_relative_path("private/data.txt"), Ok(()));
    for path in [
        "",
        "/etc/passwd",
        "../other/private/secret.txt",
        "a/../b",
        "a//b",
        "./x",
    ] {
        assert_eq!(
            profile.check_relative_path(path),
            Err(RuntimeError::PrefixEscape),
            "path {path:?}"
        );
    }
}

#[test]
fn prefix_sharing_requires_explicit_consent() {
    let mut demo = profile("org.aether.demo");
    let other = profile("org.aether.other");
    assert_eq!(
        demo.adopt_prefix(&other, false),
        Err(RuntimeError::SharingNotAuthorized)
    );
    assert_eq!(demo.prefix, "/apps/org.aether.demo");
    assert_eq!(demo.adopt_prefix(&other, true), Ok(()));
    assert_eq!(demo.prefix, "/apps/org.aether.other");
    assert!(demo.shared_prefix);
}

#[test]
fn table_rejects_duplicate_prefixes_and_keeps_profiles_distinct() {
    let mut table = RuntimeTable::<4>::new();
    assert_eq!(table.register(profile("org.aether.demo")), Ok(0));
    assert_eq!(
        table.register(profile("org.aether.demo")),
        Err(RuntimeError::PrefixInUse)
    );
    assert_eq!(table.register(profile("org.aether.other")), Ok(1));
    assert_eq!(table.len(), 2);
    assert!(table.get("org.aether.demo").is_some());
    assert!(table.get("org.example.absent").is_none());
}

#[test]
fn prefixes_look_up_the_registered_profile() {
    let mut table = RuntimeTable::<4>::new();
    assert_eq!(table.register(profile("org.aether.demo")), Ok(0));

    // The kernel-owned root must match the derived prefix exactly; another
    // application's root yields no profile and therefore no declared limits.
    let found = table.find_by_prefix("/apps/org.aether.demo");
    assert_eq!(found.map(|profile| profile.id), Some("org.aether.demo"));
    assert_eq!(found.map(|profile| profile.limits.open_files), Some(4));
    assert!(table.find_by_prefix("/apps/org.aether.other").is_none());
    assert!(table.find_by_prefix("/apps").is_none());
    assert!(
        table
            .find_by_prefix("/apps/org.aether.demo/extra")
            .is_none()
    );
}

#[test]
fn table_reports_a_full_registry() {
    let mut table = RuntimeTable::<1>::new();
    assert!(table.register(profile("org.aether.demo")).is_ok());
    assert_eq!(
        table.register(profile("org.aether.other")),
        Err(RuntimeError::TableFull)
    );
}
