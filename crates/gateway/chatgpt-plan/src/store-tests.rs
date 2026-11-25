//! Tests for the owner-only sign-in record, private directory, and lock.

use super::*;

fn active() -> Record {
    Record {
        client_id: Some("oaiapp_1".to_owned()),
        subject: Some("user-1".to_owned()),
        email: Some("a@example.test".to_owned()),
        session: Session::Active(Credentials {
            refresh_token: Secret::new("rt-secret".to_owned()),
            scopes: vec!["chatgpt.tokens.use.direct".to_owned()],
        }),
        ..Record::new("urn:uuid:00000000-0000-4000-8000-000000000000".to_owned())
    }
}

fn private_dir() -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().expect("tempdir");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).expect("chmod");
    }
    dir
}

#[cfg(unix)]
fn write_owner_only(path: &Path, bytes: &[u8]) {
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .expect("open");
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).expect("chmod");
    file.write_all(bytes).expect("write");
}

#[cfg(not(unix))]
#[test]
fn unsupported_platform_refuses_to_open() {
    let dir = private_dir();
    assert!(matches!(
        Store::open(dir.path()),
        Err(StoreError::Unsupported)
    ));
}

#[cfg(unix)]
#[test]
fn no_record_reads_as_none() {
    let dir = private_dir();
    assert!(
        Store::open(dir.path())
            .expect("opens")
            .load()
            .expect("loads")
            .is_none()
    );
}

#[cfg(unix)]
#[test]
fn a_saved_record_round_trips_owner_only_and_leaves_no_temp_file() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = private_dir();
    let store = Store::open(dir.path()).expect("opens");
    store.save(&active()).expect("saves");
    assert_eq!(store.load().expect("loads"), Some(active()));

    let store_dir = dir.path().join(STORE_DIR);
    let dir_mode = fs::symlink_metadata(&store_dir)
        .expect("dir meta")
        .permissions()
        .mode();
    assert_eq!(dir_mode & 0o777, 0o700);

    for name in [RECORD_FILE, LOCK_FILE] {
        let mode = fs::symlink_metadata(store_dir.join(name))
            .expect("file meta")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "{name}");
    }

    let mut names: Vec<_> = fs::read_dir(&store_dir)
        .expect("list")
        .map(|entry| entry.expect("entry").file_name())
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            std::ffi::OsString::from(RECORD_FILE),
            std::ffi::OsString::from(LOCK_FILE),
        ]
    );
}

#[cfg(unix)]
#[test]
fn a_0775_profile_directory_works_by_creating_a_0700_store_subdirectory() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::TempDir::new().expect("tempdir");
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o775)).expect("chmod 0775");
    let config_file = dir.path().join("gateway.toml");
    fs::write(&config_file, b"# profile config\n").expect("write");

    let store = Store::open(dir.path()).expect("opens under 0775 parent");
    store.save(&active()).expect("saves");
    assert_eq!(store.load().expect("loads"), Some(active()));

    let parent_mode = fs::symlink_metadata(dir.path())
        .expect("parent meta")
        .permissions()
        .mode();
    assert_eq!(
        parent_mode & 0o777,
        0o775,
        "parent permissions stay untouched"
    );
    let sub_mode = fs::symlink_metadata(dir.path().join(STORE_DIR))
        .expect("sub meta")
        .permissions()
        .mode();
    assert_eq!(sub_mode & 0o777, 0o700);
    assert!(config_file.exists());
}

#[cfg(unix)]
#[test]
fn a_group_writable_directory_is_refused_and_nothing_is_written() {
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};

    // Pre-existing group- or other-accessible `chatgpt-plan/` is refused on open.
    for bad_mode in [0o770, 0o750, 0o710, 0o704, 0o755] {
        let dir = private_dir();
        let store_dir = dir.path().join(STORE_DIR);
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&store_dir)
            .expect("mkdir");
        fs::set_permissions(&store_dir, fs::Permissions::from_mode(bad_mode)).expect("chmod");
        assert!(
            matches!(Store::open(dir.path()), Err(StoreError::UnsafeDirectory(_))),
            "mode {bad_mode:#o}"
        );
    }

    // Widening `chatgpt-plan/` after open is refused by both save and load.
    let dir = private_dir();
    let store = Store::open(dir.path()).expect("opens");
    let store_dir = dir.path().join(STORE_DIR);
    fs::set_permissions(&store_dir, fs::Permissions::from_mode(0o770)).expect("chmod");
    assert!(matches!(
        store.save(&active()),
        Err(StoreError::UnsafeDirectory(_))
    ));
    assert!(!store_dir.join(RECORD_FILE).exists());
    assert!(matches!(store.load(), Err(StoreError::UnsafeDirectory(_))));
}

#[cfg(unix)]
#[test]
fn a_symlinked_directory_is_refused() {
    use std::os::unix::fs::DirBuilderExt as _;

    // A symlinked profile directory is refused.
    let dir = private_dir();
    let real = dir.path().join("real");
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&real)
        .expect("mkdir");
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");
    assert!(matches!(
        Store::open(&link),
        Err(StoreError::UnsafeDirectory(_))
    ));

    // A symlinked `chatgpt-plan/` subdirectory inside a real profile directory is refused.
    let profile = private_dir();
    std::os::unix::fs::symlink(&real, profile.path().join(STORE_DIR)).expect("symlink subdir");
    assert!(matches!(
        Store::open(profile.path()),
        Err(StoreError::UnsafeDirectory(_))
    ));

    // Replacing `chatgpt-plan/` with a symlink after open is refused on load and save.
    let swapped = private_dir();
    let store = Store::open(swapped.path()).expect("opens");
    let moved = swapped.path().join("moved");
    fs::rename(swapped.path().join(STORE_DIR), &moved).expect("move");
    std::os::unix::fs::symlink(&moved, swapped.path().join(STORE_DIR)).expect("swap symlink");
    assert!(matches!(store.load(), Err(StoreError::UnsafeDirectory(_))));
    assert!(matches!(
        store.save(&active()),
        Err(StoreError::UnsafeDirectory(_))
    ));
}

#[cfg(unix)]
#[test]
fn a_symlinked_or_non_regular_record_is_refused_without_reading() {
    let dir = private_dir();
    let store = Store::open(dir.path()).expect("opens");
    let store_dir = dir.path().join(STORE_DIR);
    let record_path = store_dir.join(RECORD_FILE);

    // Even when the symlink target is a valid `0600` record owned by this user,
    // `load` refuses the symlink itself.
    let target = store_dir.join("elsewhere.json");
    let valid = serde_json::to_vec_pretty(&active()).expect("json");
    write_owner_only(&target, &valid);
    std::os::unix::fs::symlink(&target, &record_path).expect("symlink record");
    assert!(matches!(
        store.load(),
        Err(StoreError::UnsafeFile("not a regular file"))
    ));
    fs::remove_file(&record_path).expect("unlink");

    // A directory at the record path is refused as not a regular file.
    fs::create_dir(&record_path).expect("mkdir at record path");
    assert!(matches!(
        store.load(),
        Err(StoreError::UnsafeFile("not a regular file"))
    ));
}

#[cfg(unix)]
#[test]
fn a_group_or_other_accessible_record_is_refused_without_reading() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = private_dir();
    let store = Store::open(dir.path()).expect("opens");
    store.save(&active()).expect("saves");
    let record_path = dir.path().join(STORE_DIR).join(RECORD_FILE);

    for bad_mode in [0o640, 0o620, 0o604, 0o644] {
        fs::set_permissions(&record_path, fs::Permissions::from_mode(bad_mode)).expect("chmod");
        assert!(
            matches!(
                store.load(),
                Err(StoreError::UnsafeFile("accessible by group or others"))
            ),
            "mode {bad_mode:#o}"
        );
    }

    fs::set_permissions(&record_path, fs::Permissions::from_mode(0o600)).expect("restore 0600");
    assert_eq!(store.load().expect("loads"), Some(active()));
}

#[cfg(unix)]
#[test]
fn a_second_store_on_the_same_directory_is_refused_until_the_first_drops() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = private_dir();
    let first = Store::open(dir.path()).expect("first opens");
    first.save(&active()).expect("saves");

    assert!(matches!(Store::open(dir.path()), Err(StoreError::Locked)));

    drop(first);
    let second = Store::open(dir.path()).expect("reopens once unlocked");
    assert_eq!(second.load().expect("loads"), Some(active()));
    drop(second);

    // A widened or symlinked lock file is refused on open too.
    let lock_path = dir.path().join(STORE_DIR).join(LOCK_FILE);
    fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o640)).expect("chmod lock");
    assert!(matches!(
        Store::open(dir.path()),
        Err(StoreError::UnsafeFile(_))
    ));
    fs::remove_file(&lock_path).expect("remove widened lock");
    let target = dir.path().join(STORE_DIR).join("other");
    write_owner_only(&target, b"");
    std::os::unix::fs::symlink(&target, &lock_path).expect("symlink lock");
    assert!(matches!(
        Store::open(dir.path()),
        Err(StoreError::UnsafeFile(_))
    ));
}

#[cfg(unix)]
#[test]
fn a_corrupt_or_future_record_is_reported_never_guessed() {
    let dir = private_dir();
    let store = Store::open(dir.path()).expect("opens");
    let record_path = dir.path().join(STORE_DIR).join(RECORD_FILE);
    write_owner_only(&record_path, b"{ not json");
    assert!(matches!(store.load(), Err(StoreError::Corrupt)));

    let mut future = serde_json::to_value(active()).expect("value");
    future["version"] = serde_json::json!(2);
    write_owner_only(&record_path, future.to_string().as_bytes());
    assert!(matches!(store.load(), Err(StoreError::Corrupt)));
}

#[cfg(unix)]
#[test]
fn a_flat_record_is_reported_as_unreadable_never_silently_dropped() {
    let dir = private_dir();
    let store = Store::open(dir.path()).expect("opens");
    let record_path = dir.path().join(STORE_DIR).join(RECORD_FILE);

    for flat in [
        serde_json::json!({
            "version": 1,
            "agent_host_id": "urn:uuid:00000000-0000-4000-8000-000000000000",
            "client_id": "oaiapp_1",
            "subject": "user-1",
            "email": "a@example.test",
            "refresh_token": "rt-secret",
            "scopes": ["chatgpt.tokens.use.direct"],
            "status": "active",
            "refresh_dispatched": false,
            "revocation": null,
            "unconfirmed_cleanup": false,
        }),
        serde_json::json!({
            "version": 1,
            "agent_host_id": "urn:uuid:00000000-0000-4000-8000-000000000000",
            "client_id": null,
            "subject": null,
            "email": null,
            "refresh_token": null,
            "scopes": [],
            "status": "signed_out",
            "refresh_dispatched": false,
            "revocation": null,
            "unconfirmed_cleanup": false,
        }),
    ] {
        write_owner_only(&record_path, flat.to_string().as_bytes());
        assert!(matches!(store.load(), Err(StoreError::Corrupt)), "{flat}");
    }
    drop(store);

    let plan = crate::plan::ChatGptPlan::open(dir.path(), crate::oauth::OAuthConfig::openai())
        .expect("broker opens and reports the record unreadable");
    let status = plan.status();
    assert!(status.unreadable);
    assert_eq!(status.status, None);
}

#[cfg(unix)]
#[test]
fn each_session_variant_round_trips_and_inconsistent_records_are_refused() {
    let dir = private_dir();
    let store = Store::open(dir.path()).expect("opens");
    let record_path = dir.path().join(STORE_DIR).join(RECORD_FILE);

    let creds = Credentials {
        refresh_token: Secret::new("rt-secret".to_owned()),
        scopes: vec!["chatgpt.tokens.use.direct".to_owned()],
    };
    for session in [
        Session::SignedOut {
            revocation: Some(Revocation::Confirmed),
        },
        Session::Active(creds.clone()),
        Session::Refreshing(creds),
        Session::NeedsAuth {
            refresh_token: None,
        },
        Session::NeedsAuth {
            refresh_token: Some(Secret::new("rt-maybe-live".to_owned())),
        },
    ] {
        let expected = Record {
            session,
            ..active()
        };
        store.save(&expected).expect("saves");
        assert_eq!(store.load().expect("loads"), Some(expected.clone()));
        if matches!(
            expected.session,
            Session::SignedOut { .. }
                | Session::NeedsAuth {
                    refresh_token: None
                }
        ) {
            let raw = fs::read_to_string(&record_path).expect("raw");
            assert!(!raw.contains("refresh_token"), "{raw}");
        }
    }

    // A non-SignedOut session without `client_id` or `subject`, or an account
    // subject without `client_id`, is rejected as corrupt on load.
    for mut bad in [
        Record {
            client_id: None,
            ..active()
        },
        Record {
            subject: None,
            ..active()
        },
        Record {
            client_id: Some(String::new()),
            ..active()
        },
        Record {
            subject: Some(String::new()),
            ..active()
        },
        Record {
            client_id: None,
            subject: Some("user-1".to_owned()),
            session: Session::SignedOut { revocation: None },
            ..Record::new("urn:uuid:00000000-0000-4000-8000-000000000000".to_owned())
        },
    ] {
        let bytes = serde_json::to_vec_pretty(&bad).expect("json");
        write_owner_only(&record_path, &bytes);
        assert!(matches!(store.load(), Err(StoreError::Corrupt)), "{bad:?}");
        if matches!(bad.session, Session::Active(_)) {
            bad.session = Session::NeedsAuth {
                refresh_token: None,
            };
            let bytes = serde_json::to_vec_pretty(&bad).expect("json");
            write_owner_only(&record_path, &bytes);
            assert!(matches!(store.load(), Err(StoreError::Corrupt)), "{bad:?}");
        }
    }
}

#[test]
fn debug_output_never_shows_the_refresh_token() {
    let shown = format!("{:?}", active());
    assert!(!shown.contains("rt-secret"), "{shown}");
    assert!(shown.contains("<redacted>"));
}

#[cfg(unix)]
#[test]
fn remove_is_idempotent() {
    let dir = private_dir();
    let store = Store::open(dir.path()).expect("opens");
    store.remove().expect("nothing to remove");
    store.save(&active()).expect("saves");
    store.remove().expect("removes");
    assert!(store.load().expect("loads").is_none());
}

#[cfg(unix)]
#[test]
fn a_temp_file_left_by_a_crash_is_removed_by_load_and_by_save() {
    use std::os::unix::fs::DirBuilderExt as _;

    let dir = private_dir();
    let store_dir = dir.path().join(STORE_DIR);
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&store_dir)
        .expect("mkdir");
    let crashed_before_open = store_dir.join(".chatgpt-plan.json.before.tmp");
    write_owner_only(&crashed_before_open, b"{\"refresh_token\":\"rt-secret\"}");

    // `Store::open` reaps crash residue once it holds the lock.
    let store = Store::open(dir.path()).expect("opens");
    assert!(!crashed_before_open.exists(), "open reaps the stale temp");

    // A contended second opener fails before `reap_stale_temps` can touch an
    // active temp file belonging to the lock holder.
    let in_flight = store_dir.join(".chatgpt-plan.json.inflight.tmp");
    write_owner_only(&in_flight, b"{\"refresh_token\":\"rt-live\"}");
    assert!(matches!(Store::open(dir.path()), Err(StoreError::Locked)));
    assert!(
        in_flight.exists(),
        "a locked-out opener never reaps under the holder's feet"
    );

    let unrelated = store_dir.join("notes.txt");
    write_owner_only(&unrelated, b"x");
    assert!(store.load().expect("loads").is_none());
    assert!(!in_flight.exists(), "load reaps the stale temp");

    let stale = store_dir.join(".chatgpt-plan.json.deadbeef.tmp");
    write_owner_only(&stale, b"{}");
    store.save(&active()).expect("saves");
    assert!(!stale.exists(), "save reaps the stale temp");
    assert!(unrelated.exists(), "other files are untouched");
}
