//! Tests for what the broker does when the disk or the issuer fails it
//! part-way: a save that fails, and a revocation that is not confirmed.

use super::*;

fn refresh_count(rig: &Rig) -> usize {
    rig.fake
        .grants()
        .iter()
        .filter(|grant| *grant == "refresh_token")
        .count()
}

#[cfg(unix)]
fn set_mode(rig: &Rig, mode: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(
        rig.dir.path().join(crate::store::STORE_DIR),
        std::fs::Permissions::from_mode(mode),
    )
    .expect("chmod");
}

#[cfg(unix)]
#[tokio::test]
async fn a_marker_that_cannot_be_saved_sends_nothing_and_changes_nothing() {
    let rig = Rig::seeded(Some(active())).await;
    set_mode(&rig, 0o500);
    assert!(matches!(
        rig.plan.access_token().await,
        Err(PlanError::Store(_))
    ));
    assert_eq!(refresh_count(&rig), 0, "no refresh was sent");
    set_mode(&rig, 0o700);
    // Nothing was published either: the next request refreshes normally.
    assert_eq!(
        rig.plan.access_token().await.expect("a token").expose(),
        "at-1"
    );
    assert!(matches!(rig.record().session, Session::Active(_)));
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rotation_that_cannot_be_saved_is_revoked_and_needs_a_new_sign_in() {
    let rig = Rig::seeded(Some(active())).await;
    rig.fake.set(|behavior| behavior.hold = Some(Held::Refresh));
    let caller = {
        let plan = rig.plan.clone();
        tokio::spawn(async move { plan.access_token().await })
    };
    rig.fake.refresh_arrived().await;
    // The marker is saved and the request is open. Now the disk refuses.
    set_mode(&rig, 0o500);
    rig.fake.release();
    assert!(matches!(
        caller.await.expect("joins"),
        Err(PlanError::Store(_))
    ));
    set_mode(&rig, 0o700);

    // The unsaved token was revoked, and this process no longer trusts the
    // sign-in. The disk still holds the marker, so a restart agrees.
    assert_eq!(rig.fake.seen(|seen| seen.revoked.clone()), ["rt-1"]);
    assert_eq!(rig.plan.status().status, Some(Status::NeedsAuth));
    assert!(matches!(
        rig.plan.access_token().await,
        Err(PlanError::NeedsAuth)
    ));
    assert_eq!(refresh_count(&rig), 1, "the spent token is not tried again");
    assert!(matches!(rig.record().session, Session::Refreshing(_)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cleanup_the_issuer_does_not_confirm_stays_visible() {
    let rig = Rig::seeded(Some(active())).await;
    rig.fake.set(|behavior| behavior.hold = Some(Held::Refresh));
    let caller = {
        let plan = rig.plan.clone();
        tokio::spawn(async move { plan.access_token().await })
    };
    rig.fake.refresh_arrived().await;
    assert!(rig.plan.sign_out().await.expect("signs out").revoked);
    assert!(!rig.plan.status().cleanup_unconfirmed);

    // The late refresh's token cannot be kept. This time the issuer does
    // not confirm its revocation.
    rig.fake.set(|behavior| behavior.revoke_status = 503);
    rig.fake.release();
    assert!(caller.await.expect("joins").is_err());

    let status = rig.plan.status();
    assert!(status.cleanup_unconfirmed, "a token may still be live");
    assert_eq!(
        status.revocation,
        Some(Revocation::Confirmed),
        "the sign-out's own outcome is not rewritten"
    );
    assert!(
        rig.record().unconfirmed_cleanup,
        "and it survives a restart"
    );
    assert!(
        rig.record().session.refresh_token().is_none(),
        "the token is not kept either way"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cleanup_failure_after_reset_stays_visible() {
    let rig = Rig::seeded(Some(active())).await;
    rig.fake.set(|behavior| behavior.hold = Some(Held::Refresh));
    let caller = {
        let plan = rig.plan.clone();
        tokio::spawn(async move { plan.access_token().await })
    };
    rig.fake.refresh_arrived().await;

    // Reset removes the local record while the refresh is still open.
    rig.plan.reset().expect("resets");
    assert_eq!(rig.plan.status().status, None);
    assert!(!rig.plan.status().cleanup_unconfirmed);

    // The late refresh returns a rotated token, and its revocation fails.
    rig.fake.set(|behavior| behavior.revoke_status = 503);
    rig.fake.release();
    assert!(matches!(
        caller.await.expect("joins"),
        Err(PlanError::Superseded)
    ));

    assert_eq!(rig.fake.seen(|seen| seen.revoked.clone()), ["rt-1"]);
    assert!(
        rig.plan.status().cleanup_unconfirmed,
        "a token we did not keep may still be live at OpenAI"
    );
    let warning = rig.record();
    assert!(warning.unconfirmed_cleanup, "persisted across restarts");
    assert!(warning.session.refresh_token().is_none());
    assert!(warning.client_id.is_none() && warning.subject.is_none());

    // Starting and completing a new sign-in does not erase the warning.
    rig.fake.set(|behavior| behavior.revoke_status = 200);
    let callback = rig.start(Some("oaiapp_2")).await;
    assert!(rig.plan.status().cleanup_unconfirmed);
    rig.plan
        .complete_sign_in(callback)
        .await
        .expect("the new install signs in");
    assert!(rig.plan.status().cleanup_unconfirmed);
    assert!(rig.record().unconfirmed_cleanup);

    // Reopening the store preserves the warning; only a subsequent reset
    // acknowledges and clears it.
    let Rig {
        fake, dir, plan, ..
    } = rig;
    drop(plan);
    let reopened = ChatGptPlan::open(dir.path(), fake.config()).expect("reopens");
    assert!(reopened.status().cleanup_unconfirmed);
    reopened.reset().expect("resets");
    assert!(!reopened.status().cleanup_unconfirmed);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cleanup_failure_after_reset_reports_honestly_even_when_unwritable() {
    let rig = Rig::seeded(Some(active())).await;
    rig.fake.set(|behavior| behavior.hold = Some(Held::Refresh));
    let caller = {
        let plan = rig.plan.clone();
        tokio::spawn(async move { plan.access_token().await })
    };
    rig.fake.refresh_arrived().await;
    rig.plan.reset().expect("resets");

    // Neither the revocation nor the warning write succeeds.
    set_mode(&rig, 0o500);
    rig.fake.set(|behavior| behavior.revoke_status = 503);
    rig.fake.release();
    assert!(matches!(
        caller.await.expect("joins"),
        Err(PlanError::Superseded)
    ));
    assert!(
        rig.plan.status().cleanup_unconfirmed,
        "in-memory status stays truthful when the warning write fails"
    );

    // Once writable again, starting a new sign-in carries the warning onto disk.
    set_mode(&rig, 0o700);
    let _callback = rig.start(Some("oaiapp_2")).await;
    assert!(rig.plan.status().cleanup_unconfirmed);
    assert!(rig.record().unconfirmed_cleanup);
}

#[tokio::test]
async fn a_rejected_non_rotating_refresh_keeps_the_old_token_for_revocation() {
    let rig = Rig::seeded(Some(active())).await;
    rig.fake.set(|behavior| {
        behavior.omit_refresh_token_on_refresh = true;
        behavior.jwks_status = 503;
    });
    assert!(matches!(
        rig.plan.access_token().await,
        Err(PlanError::NeedsAuth)
    ));
    assert_eq!(
        rig.record().session,
        Session::NeedsAuth {
            refresh_token: Some(Secret::new("rt-seed".to_owned())),
        },
        "the still-live old token stays in custody for revocation"
    );

    // The retained token is never used for another refresh.
    rig.fake.set(|behavior| behavior.jwks_status = 200);
    assert!(matches!(
        rig.plan.access_token().await,
        Err(PlanError::NeedsAuth)
    ));
    assert_eq!(refresh_count(&rig), 1);

    // Signing out revokes the retained old token at the issuer.
    assert_eq!(
        rig.plan.sign_out().await.expect("signs out"),
        SignedOut { revoked: true }
    );
    assert_eq!(rig.fake.seen(|seen| seen.revoked.clone()), ["rt-seed"]);
    assert!(rig.fake.seen(|seen| seen.valid_refresh_tokens.is_empty()));
    assert_eq!(
        rig.record().session,
        Session::SignedOut {
            revocation: Some(Revocation::Confirmed),
        }
    );
}

#[tokio::test]
async fn an_unconfirmed_sign_out_revocation_survives_a_new_active_sign_in_and_restart() {
    let rig = Rig::seeded(Some(active())).await;
    rig.fake.set(|behavior| behavior.revoke_status = 503);
    assert_eq!(
        rig.plan.sign_out().await.expect("signs out"),
        SignedOut { revoked: false }
    );
    assert_eq!(
        rig.plan.forget().await.expect("forgets after sign out"),
        SignedOut { revoked: false }
    );

    rig.fake.set(|behavior| {
        behavior.revoke_status = 200;
        behavior.subject = "user-2".to_owned();
    });
    let callback = rig.start(Some("oaiapp_2")).await;
    rig.plan
        .complete_sign_in(callback)
        .await
        .expect("new account activates");

    let status = rig.plan.status();
    assert_eq!(status.status, Some(Status::Active));
    assert!(
        status.cleanup_unconfirmed,
        "unconfirmed revocation from retired session stays visible while active"
    );
    assert!(rig.record().unconfirmed_cleanup);

    let Rig {
        fake, dir, plan, ..
    } = rig;
    drop(plan);
    let reopened = ChatGptPlan::open(dir.path(), fake.config()).expect("reopens");
    assert_eq!(reopened.status().status, Some(Status::Active));
    assert!(reopened.status().cleanup_unconfirmed);
}

#[tokio::test]
async fn same_account_re_sign_in_invalidates_an_older_session_fence() {
    let rig = Rig::new().await;
    rig.sign_in().await;
    let old_fence = rig.plan.current_fence().expect("active fence");
    assert!(rig.plan.verify_fence(&old_fence));

    // Same subject (`user-1`) signs out and signs back in: subject, active
    // session, and plan scope all match, and only `generation` differs.
    rig.plan.sign_out().await.expect("signs out");
    let callback = rig.start(None).await;
    rig.plan
        .complete_sign_in(callback)
        .await
        .expect("same account signs back in");

    assert_eq!(rig.plan.status().status, Some(Status::Active));
    assert!(
        !rig.plan.verify_fence(&old_fence),
        "fence from before same-account sign-out/sign-in must not verify"
    );
    let mut ran = false;
    assert_eq!(
        rig.plan.commit_fenced(
            &old_fence,
            || "stale",
            || {
                ran = true;
                Ok::<_, &str>(())
            }
        ),
        Err("stale")
    );
    assert!(!ran, "stale commit closure must not execute");
}

#[tokio::test]
async fn commit_fenced_holds_broker_lock_while_sign_out_waits() {
    let Rig {
        fake, dir, plan, ..
    } = Rig::seeded(Some(active())).await;
    let plan = Arc::new(plan);
    let fence = plan.current_fence().expect("active fence");

    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let commit_plan = Arc::clone(&plan);
    let commit_fence = fence.clone();
    let commit_thread = std::thread::spawn(move || {
        commit_plan.commit_fenced(
            &commit_fence,
            || "stale",
            || {
                entered_tx.send(()).expect("signal entered");
                release_rx.recv().expect("wait for release");
                Ok::<_, &'static str>("committed")
            },
        )
    });
    entered_rx.recv().expect("commit entered");

    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let sign_out_plan = Arc::clone(&plan);
    let sign_out_thread = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("sign_out runtime");
        started_tx.send(()).expect("signal sign_out starting");
        let out = rt.block_on(sign_out_plan.sign_out());
        done_tx.send(out).expect("send sign_out result");
    });
    started_rx.recv().expect("sign_out thread started");
    assert!(
        done_rx.recv_timeout(Duration::from_millis(40)).is_err(),
        "sign_out must wait on the broker lock until commit_fenced finishes"
    );
    assert!(fake.seen(|seen| seen.revoked.is_empty()));
    let _keep_dir = &dir;
    let mid_record = plan.inner.store.load().expect("load").expect("record");
    assert!(matches!(mid_record.session, Session::Active(_)));

    release_tx.send(()).expect("release commit");
    assert_eq!(commit_thread.join().expect("commit join"), Ok("committed"));
    // Drive the current-thread runtime while sign_out_thread finishes its async revoke.
    let sign_out_res = tokio::task::spawn_blocking(move || {
        let out = done_rx.recv().expect("sign_out done");
        sign_out_thread.join().expect("sign_out join");
        out
    })
    .await
    .expect("blocking join");
    assert_eq!(
        sign_out_res.expect("signs out"),
        SignedOut { revoked: true }
    );
    assert_eq!(fake.seen(|seen| seen.revoked.clone()), ["rt-seed"]);
    assert_eq!(
        plan.commit_fenced(&fence, || "stale", || Ok::<_, &str>(())),
        Err("stale")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn store_io_and_interrupted_errors_preserve_phase_and_source() {
    let rig = Rig::seeded(None).await;
    set_mode(&rig, 0o500);
    let err = rig
        .plan
        .begin_sign_in("http://127.0.0.1:0/auth/callback")
        .await
        .expect_err("read-only store refuses initial record write");
    set_mode(&rig, 0o700);

    match &err {
        PlanError::Store(crate::store::StoreError::Io { operation, .. }) => {
            assert_eq!(*operation, "create temp record file");
        }
        other => panic!("unexpected error: {other:?}"),
    }
    let msg = err.to_string();
    assert!(msg.contains("create temp record file"), "{msg}");
    let store_source = std::error::Error::source(&err).expect("store io source");
    assert!(!msg.contains(&store_source.to_string()), "{msg}");

    let rng_msg = PlanError::Interrupted("system randomness unavailable").to_string();
    let task_msg = PlanError::Interrupted("token refresh task stopped").to_string();
    assert_ne!(rng_msg, task_msg);
    assert!(rng_msg.contains("system randomness unavailable"));
    assert!(task_msg.contains("token refresh task stopped"));
}
