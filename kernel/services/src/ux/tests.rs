// SPDX-License-Identifier: Apache-2.0
//! Host tests for the unified filesystem user-experience policy.

use super::*;

fn app(id: &str) -> AppId {
    AppId::new(id).expect("valid app id")
}

fn fixture() -> (ResourceTable, GrantTable, AppId, AppId) {
    let notes = app("org.aether.notes");
    let other = app("org.aether.other");
    let mut resources = ResourceTable::new();
    let users = resources
        .register(
            ResourceId::NONE,
            Owner::User,
            "Users",
            ResourceKind::Directory,
        )
        .expect("users");
    let documents = resources
        .register(users, Owner::User, "Documents", ResourceKind::Directory)
        .expect("documents");
    resources
        .register(documents, Owner::User, "Report.odt", ResourceKind::File)
        .expect("report");
    resources
        .register(documents, Owner::User, "Budget.csv", ResourceKind::File)
        .expect("budget");
    let apps = resources
        .register(
            ResourceId::NONE,
            Owner::System,
            "Applications",
            ResourceKind::Directory,
        )
        .expect("applications");
    let notes_root = resources
        .register(
            apps,
            Owner::Application(notes),
            "org.aether.notes",
            ResourceKind::Directory,
        )
        .expect("notes root");
    resources
        .register(
            notes_root,
            Owner::Application(notes),
            "Data",
            ResourceKind::Directory,
        )
        .expect("notes data");
    let other_root = resources
        .register(
            apps,
            Owner::Application(other),
            "org.aether.other",
            ResourceKind::Directory,
        )
        .expect("other root");
    resources
        .register(
            other_root,
            Owner::Application(other),
            "Data",
            ResourceKind::Directory,
        )
        .expect("other data");
    (resources, GrantTable::new(), notes, other)
}

fn by_name(resources: &ResourceTable, name: &str) -> ResourceId {
    resources
        .slots
        .iter()
        .flatten()
        .find(|resource| resource.name() == name)
        .expect("named resource")
        .id
}

#[test]
fn identity_survives_rename_and_move() {
    let (mut resources, _, _, _) = fixture();
    let report = by_name(&resources, "Report.odt");
    let documents = by_name(&resources, "Documents");
    let users = by_name(&resources, "Users");

    resources.rename(report, "Plan.odt").expect("rename");
    assert_eq!(resources.get(report).expect("resource").name(), "Plan.odt");
    resources.move_to(report, users).expect("move");
    assert_eq!(resources.get(report).expect("resource").parent(), users);
    assert_eq!(resources.get(report).expect("resource").id(), report);

    let other = by_name(&resources, "Budget.csv");
    assert_ne!(report, other);
    resources
        .rename(other, "Plan.odt")
        .expect("duplicate name in another folder is fine");
    resources
        .move_to(other, users)
        .expect_err("duplicate in same parent");
    let _ = documents;
}

#[test]
fn cycles_and_deleted_identity_fail_closed() {
    let (mut resources, _, _, _) = fixture();
    let users = by_name(&resources, "Users");
    let documents = by_name(&resources, "Documents");
    assert_eq!(resources.move_to(users, documents), Err(UxError::Cycle));
    assert_eq!(resources.move_to(users, users), Err(UxError::Cycle));

    resources.remove(users).expect("remove");
    assert_eq!(resources.get(users), Err(UxError::NotFound));
    assert_eq!(
        resources.register(users, Owner::User, "orphan", ResourceKind::File),
        Err(UxError::NotFound),
        "a deleted parent must not accept new children"
    );
}

#[test]
fn selected_file_grant_never_reaches_parent_or_siblings() {
    let (resources, mut grants, notes, _) = fixture();
    let report = by_name(&resources, "Report.odt");
    let budget = by_name(&resources, "Budget.csv");
    let documents = by_name(&resources, "Documents");

    grants
        .grant(
            &resources,
            notes,
            report,
            GrantScope::File,
            Rights::READ,
            GrantLifetime::OneTime,
        )
        .expect("grant");
    assert!(grants.authorizes(&resources, notes, report, Rights::READ));
    assert!(!grants.authorizes(&resources, notes, report, Rights::WRITE));
    assert!(!grants.authorizes(&resources, notes, budget, Rights::READ));
    assert!(!grants.authorizes(&resources, notes, documents, Rights::ENUMERATE));
    assert!(!grants.authorizes(&resources, notes, documents, Rights::READ));
}

#[test]
fn folder_grant_is_scoped_to_its_subtree_and_revocable() {
    let (mut resources, mut grants, notes, _) = fixture();
    let documents = by_name(&resources, "Documents");
    let report = by_name(&resources, "Report.odt");
    let users = by_name(&resources, "Users");

    let slot = grants
        .grant(
            &resources,
            notes,
            documents,
            GrantScope::Folder,
            Rights::READ | Rights::ENUMERATE | Rights::CREATE,
            GrantLifetime::Persistent,
        )
        .expect("folder grant");
    assert!(grants.authorizes(&resources, notes, report, Rights::READ));
    assert!(grants.authorizes(&resources, notes, documents, Rights::ENUMERATE));
    assert!(!grants.authorizes(&resources, notes, users, Rights::READ));

    resources
        .register(documents, Owner::User, "New.odt", ResourceKind::File)
        .expect("new document");
    let new = by_name(&resources, "New.odt");
    assert!(
        grants.authorizes(&resources, notes, new, Rights::READ),
        "a folder grant covers children created later inside the scope"
    );

    assert!(grants.revoke(slot));
    assert!(!grants.authorizes(&resources, notes, report, Rights::READ));
    assert!(!grants.revoke(slot), "second revocation is a no-op");
}

#[test]
fn one_time_and_persistent_lifetimes_differ() {
    let (resources, mut grants, notes, _) = fixture();
    let report = by_name(&resources, "Report.odt");
    grants
        .grant(
            &resources,
            notes,
            report,
            GrantScope::File,
            Rights::READ,
            GrantLifetime::OneTime,
        )
        .expect("one time");
    assert!(grants.consume(&resources, notes, report, Rights::READ));
    assert!(
        !grants.consume(&resources, notes, report, Rights::READ),
        "a one-time grant is spent after the operation"
    );

    let persistent = grants
        .grant(
            &resources,
            notes,
            report,
            GrantScope::File,
            Rights::READ | Rights::WRITE,
            GrantLifetime::Persistent,
        )
        .expect("persistent");
    assert!(grants.consume(&resources, notes, report, Rights::WRITE));
    assert!(grants.consume(&resources, notes, report, Rights::WRITE));
    assert!(grants.revoke(persistent));
    assert!(!grants.authorizes(&resources, notes, report, Rights::READ));
}

#[test]
fn applications_cannot_browse_another_private_vault() {
    let (resources, mut grants, notes, other) = fixture();
    let notes_data = by_name(&resources, "org.aether.notes");
    let other_data = by_name(&resources, "org.aether.other");

    assert!(can_see(
        &resources,
        &grants,
        Requester::Application(notes),
        notes_data
    ));
    assert!(!can_see(
        &resources,
        &grants,
        Requester::Application(notes),
        other_data
    ));
    assert!(!can_see(&resources, &grants, Requester::User, notes_data));
    assert_eq!(
        grants.grant(
            &resources,
            notes,
            other_data,
            GrantScope::Folder,
            Rights::READ,
            GrantLifetime::Persistent,
        ),
        Err(UxError::Denied),
        "one application cannot be granted another's private vault"
    );
    assert!(!can_see(
        &resources,
        &grants,
        Requester::Application(other),
        notes_data
    ));
}

#[test]
fn search_is_filtered_before_results_are_emitted() {
    let (resources, mut grants, notes, _) = fixture();
    let docs = by_name(&resources, "Documents");
    let report = by_name(&resources, "Report.odt");
    let mut out = [ResourceId::NONE; MAX_SEARCH_RESULTS];

    let mut count = search(&resources, &grants, Requester::User, "report", &mut out);
    assert_eq!(count, 1);
    assert_eq!(out[0], report);
    assert!(
        search(
            &resources,
            &grants,
            Requester::User,
            "aether.notes",
            &mut out
        ) == 0,
        "user search must not reveal private application storage"
    );

    count = search(
        &resources,
        &grants,
        Requester::Application(notes),
        "report",
        &mut out,
    );
    assert_eq!(count, 0);
    grants
        .grant(
            &resources,
            notes,
            docs,
            GrantScope::Folder,
            Rights::READ | Rights::ENUMERATE,
            GrantLifetime::Persistent,
        )
        .expect("grant");
    count = search(
        &resources,
        &grants,
        Requester::Application(notes),
        "report",
        &mut out,
    );
    assert_eq!(count, 1);
    assert_eq!(out[0], report);
    assert_eq!(
        search(
            &resources,
            &grants,
            Requester::Application(notes),
            "org.aether.other",
            &mut out
        ),
        0
    );
}

#[test]
fn cleanup_and_uninstall_never_destroy_persistent_user_data() {
    let mut storage = AppStorage::new(app("org.aether.notes"), 100);
    assert!(storage.charge(StorageKind::Data, 30));
    assert!(storage.charge(StorageKind::Cache, 20));
    assert!(storage.charge(StorageKind::Temporary, 10));
    assert!(!storage.charge(StorageKind::Data, 100), "quota is enforced");
    assert_eq!(storage.cleanup(StorageKind::Cache), 20);
    assert_eq!(storage.cleanup(StorageKind::Temporary), 10);
    assert_eq!(storage.usage.data, 30);
    assert_eq!(storage.cleanup(StorageKind::Data), 0);

    let mut grants = GrantTable::new();
    let plan = plan_uninstall(&mut grants, &storage, false);
    assert!(plan.remove_package);
    assert!(plan.retain_private_data);
    assert!(plan.preserve_user_documents);

    let plan = plan_uninstall(&mut grants, &storage, true);
    assert!(!plan.retain_private_data);
    assert!(plan.preserve_user_documents);
}

#[test]
fn recent_removal_does_not_delete_the_resource() {
    let (resources, _, _, _) = fixture();
    let report = by_name(&resources, "Report.odt");
    let budget = by_name(&resources, "Budget.csv");
    let mut recent = Recent::new();
    recent.record(report);
    recent.record(budget);
    recent.record(report);
    assert_eq!(recent.len(), 2);
    assert!(recent.remove(report));
    assert_eq!(recent.len(), 1);
    assert!(resources.get(report).is_ok());
    recent.clear();
    assert!(recent.is_empty());
    assert!(resources.get(budget).is_ok());
}

#[test]
fn navigation_matches_the_specification_order() {
    assert_eq!(LOCATIONS.len(), 13);
    assert_eq!(LOCATIONS[0].label(), "Home");
    assert_eq!(LOCATIONS[0].path(), "/home");
    assert_eq!(LOCATIONS[9].label(), "Shared");
    assert_eq!(LOCATIONS[12].label(), "Trash");
    assert!(LOCATIONS[5].recoverable_delete());
    assert!(!LOCATIONS[12].recoverable_delete());
}

#[test]
fn duplicate_application_names_are_not_identities() {
    assert!(AppId::new("org.aether.notes").is_some());
    for invalid in ["", ".", "..", "org/aether", "org aether", "org\0aether"] {
        assert!(AppId::new(invalid).is_none(), "{invalid}");
    }
}
