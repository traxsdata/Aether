// SPDX-License-Identifier: Apache-2.0
//! Host tests for the compatibility database and classification rules.

use crate::{
    Classification, Database, DatabaseError, Outcome, TestResult, classify, shipped_database,
};

#[test]
fn shipped_database_is_consistent() {
    let database = shipped_database().expect("shipped database parses");
    assert!(database.len() >= 3);
    assert_eq!(database.count(Classification::Verified), 3);
    assert_eq!(database.count(Classification::Unsupported), 2);
    assert!(database.get("org.aether.demo").is_some());
    for entry in database.entries() {
        match entry.classification {
            Classification::Verified | Classification::Compatible => {
                assert!(entry.test_count() > 0, "{} names no test", entry.id);
            }
            Classification::Unsupported => {
                assert_ne!(entry.limitations, "none", "{} has no reason", entry.id);
            }
            Classification::Limited => {}
        }
    }
}

#[test]
fn records_must_identify_the_application_and_the_test_environment() {
    let database = Database::parse(
        "# comment\n\
         org.aether.demo|Demo|0.2.0|native|qemu-q35-ovmf|2026-10-19|verified|suite.startup|none\n",
    )
    .expect("parse");
    let entry = database.get("org.aether.demo").unwrap();
    assert_eq!(entry.version, "0.2.0");
    assert_eq!(entry.runtime, "native");
    assert_eq!(entry.platform, "qemu-q35-ovmf");
    assert_eq!(entry.tested, "2026-10-19");
    assert_eq!(entry.test_count(), 1);
}

#[test]
fn duplicate_identities_are_rejected() {
    let text = "org.aether.demo|Demo|0.2.0|native|qemu|2026-10-19|verified|t|none\n\
                org.aether.demo|Demo again|0.2.0|native|qemu|2026-10-19|verified|t|none\n";
    assert_eq!(
        Database::parse(text).err(),
        Some(DatabaseError::DuplicateIdentity)
    );
}

#[test]
fn malformed_records_are_rejected() {
    let cases = [
        (
            "org.aether.demo|Demo|0.2.0|native|qemu|2026-10-19|verified|t\n",
            DatabaseError::FieldCount,
        ),
        (
            "org.aether.demo|Demo|0.2.0|native|qemu|2026-10-19|verified|t|none|extra\n",
            DatabaseError::FieldCount,
        ),
        (
            "Demo|Demo|0.2.0|native|qemu|2026-10-19|verified|t|none\n",
            DatabaseError::InvalidIdentity,
        ),
        (
            "org.aether.demo|Demo|0.2.0|native|qemu|19-10-2026|verified|t|none\n",
            DatabaseError::InvalidDate,
        ),
        (
            "org.aether.demo|Demo|0.2.0|native|qemu|2026-10-19|excellent|t|none\n",
            DatabaseError::UnknownClassification,
        ),
        (
            "org.aether.demo|Demo|0.2.0|native|qemu|2026-10-19|verified|none|none\n",
            DatabaseError::MissingTests,
        ),
        (
            "org.example.app|App|1.0|none|qemu|2026-10-19|unsupported|none|none\n",
            DatabaseError::MissingReason,
        ),
        (
            "org.aether.demo||0.2.0|native|qemu|2026-10-19|verified|t|none\n",
            DatabaseError::InvalidField,
        ),
    ];
    for (text, expected) in cases {
        assert_eq!(Database::parse(text).err(), Some(expected), "{text}");
    }
}

#[test]
fn classification_follows_the_test_suite() {
    let passed = [
        TestResult {
            name: "functional.startup",
            outcome: Outcome::Passed,
        },
        TestResult {
            name: "security.file.sandbox",
            outcome: Outcome::Passed,
        },
    ];
    assert_eq!(classify(&passed), Classification::Verified);

    let mut limited = passed;
    limited[0].outcome = Outcome::PassedWithLimitations;
    assert_eq!(classify(&limited), Classification::Compatible);

    let mut failing = passed;
    failing[0].outcome = Outcome::Failed;
    assert_eq!(classify(&failing), Classification::Limited);

    let mut insecure = passed;
    insecure[1].outcome = Outcome::Failed;
    assert_eq!(classify(&insecure), Classification::Unsupported);

    assert_eq!(classify(&[]), Classification::Unsupported);
}
