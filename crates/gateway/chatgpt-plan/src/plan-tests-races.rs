//! Tests for what can happen while a sign-in's code exchange is open at the
//! issuer. Each holds the exchange at the fake, so the order is forced.

use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_issued_client_is_saved_before_the_exchange() {
    let rig = Rig::new().await;
    let callback = rig.start(Some("oaiapp_1")).await;
    rig.fake
        .set(|behavior| behavior.hold = Some(Held::Exchange));
    let completing = {
        let plan = rig.plan.clone();
        tokio::spawn(async move { plan.complete_sign_in(callback).await })
    };
    rig.fake.exchange_arrived().await;

    // The exchange is open at the issuer and has not answered. The issued
    // client id is already on disk.
    let record = rig.record();
    assert_eq!(record.client_id.as_deref(), Some("oaiapp_1"));
    assert_eq!(record.session, Session::SignedOut { revocation: None });

    rig.fake.release();
    completing.await.expect("joins").expect("signs in");
    assert!(matches!(rig.record().session, Session::Active(_)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_attempt_that_expires_during_the_exchange_is_refused_and_revoked() {
    let rig = Rig::new().await;
    let callback = rig.start(Some("oaiapp_1")).await;
    rig.fake
        .set(|behavior| behavior.hold = Some(Held::Exchange));
    let completing = {
        let plan = rig.plan.clone();
        tokio::spawn(async move { plan.complete_sign_in(callback).await })
    };
    rig.fake.exchange_arrived().await;

    // The attempt was valid when the exchange was sent. It expires while
    // the issuer is still answering.
    rig.advance(601);
    rig.fake.release();
    assert!(matches!(
        completing.await.expect("joins"),
        Err(PlanError::Superseded)
    ));
    assert_eq!(rig.plan.status().status, Some(Status::SignedOut));
    assert!(
        !rig.plan.status().sign_in_pending,
        "a sign-in that failed leaves nothing pending"
    );
    assert!(rig.record().session.refresh_token().is_none());
    assert_eq!(rig.fake.seen(|seen| seen.revoked.clone()), ["rt-1"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sign_out_during_the_exchange_wins_and_the_issued_token_is_revoked() {
    let rig = Rig::new().await;
    let callback = rig.start(Some("oaiapp_1")).await;
    rig.fake
        .set(|behavior| behavior.hold = Some(Held::Exchange));
    let completing = {
        let plan = rig.plan.clone();
        tokio::spawn(async move { plan.complete_sign_in(callback).await })
    };
    rig.fake.exchange_arrived().await;

    // Nothing is held yet, so the sign-out has nothing to revoke.
    assert!(!rig.plan.sign_out().await.expect("signs out").revoked);
    assert!(
        !rig.plan.status().sign_in_pending,
        "the sign-out ended the sign-in, though its exchange is still open"
    );
    rig.fake.release();
    assert!(matches!(
        completing.await.expect("joins"),
        Err(PlanError::Superseded)
    ));
    assert_eq!(rig.plan.status().status, Some(Status::SignedOut));
    assert!(
        rig.record().session.refresh_token().is_none(),
        "the late sign-in was not activated"
    );
    assert_eq!(rig.fake.seen(|seen| seen.revoked.clone()), ["rt-1"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_newer_sign_in_supersedes_one_whose_exchange_was_already_sent() {
    let rig = Rig::new().await;
    let first = rig.start(Some("oaiapp_1")).await;
    rig.fake
        .set(|behavior| behavior.hold = Some(Held::Exchange));
    let completing = {
        let plan = rig.plan.clone();
        tokio::spawn(async move { plan.complete_sign_in(first).await })
    };
    rig.fake.exchange_arrived().await;

    // The first attempt is claimed and exchanging. A second one starts.
    let second = rig.start(None).await;
    rig.fake.release();
    assert!(matches!(
        completing.await.expect("joins"),
        Err(PlanError::Superseded)
    ));
    assert_ne!(rig.plan.status().status, Some(Status::Active));
    assert!(
        rig.plan.status().sign_in_pending,
        "the second attempt is still waiting"
    );
    assert_eq!(rig.fake.seen(|seen| seen.revoked.clone()), ["rt-1"]);

    // And the second attempt can still complete.
    rig.plan
        .complete_sign_in(second)
        .await
        .expect("the newer sign-in completes");
    assert_eq!(rig.plan.status().status, Some(Status::Active));
    assert_eq!(
        rig.record().session.refresh_token().expect("held").expose(),
        "rt-2"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_forget_still_revoking_does_not_touch_a_sign_in_that_follows_it() {
    let rig = Rig::new().await;
    rig.sign_in().await;
    rig.fake.set(|behavior| behavior.hold = Some(Held::Revoke));
    let forgetting = {
        let plan = rig.plan.clone();
        tokio::spawn(async move { plan.forget().await })
    };
    rig.fake.revoke_arrived().await;

    // The account is forgotten here, and its revocation is still open at
    // the issuer. A new sign-in registers and completes meanwhile.
    assert!(rig.record().client_id.is_none());
    let callback = rig.start(Some("oaiapp_2")).await;
    rig.plan
        .complete_sign_in(callback)
        .await
        .expect("a new sign-in completes");

    rig.fake.release();
    assert!(forgetting.await.expect("joins").expect("forgets").revoked);
    let record = rig.record();
    assert!(
        matches!(record.session, Session::Active(_)),
        "the new session is untouched"
    );
    assert_eq!(record.client_id.as_deref(), Some("oaiapp_2"));
    assert_eq!(
        record.session.refresh_token().expect("held").expose(),
        "rt-2"
    );
    assert_eq!(record.session.revocation(), None);
    assert_eq!(rig.fake.seen(|seen| seen.revoked.clone()), ["rt-1"]);
}

#[cfg(unix)]
#[tokio::test]
async fn a_client_id_that_cannot_be_saved_is_not_treated_as_registered() {
    use std::os::unix::fs::PermissionsExt as _;

    let rig = Rig::new().await;
    let callback = rig.start(Some("oaiapp_1")).await;
    let lock = |mode| {
        std::fs::set_permissions(
            rig.dir.path().join(crate::store::STORE_DIR),
            std::fs::Permissions::from_mode(mode),
        )
        .expect("chmod");
    };
    lock(0o500);
    assert!(matches!(
        rig.plan.complete_sign_in(callback).await,
        Err(PlanError::Store(_))
    ));
    lock(0o700);
    assert!(rig.fake.grants().is_empty(), "nothing was exchanged");
    assert!(rig.record().client_id.is_none());

    // The next sign-in still registers: the unsaved id was never published.
    let url = rig.plan.begin_sign_in(REDIRECT).await.expect("begins");
    assert!(url.contains("client_id=dynamic_agent_client"), "{url}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_held_sign_out_that_succeeds_after_forget_and_re_sign_in_raises_no_warning() {
    let rig = Rig::new().await;
    rig.sign_in().await;
    rig.fake.set(|behavior| {
        behavior.hold = Some(Held::Revoke);
        behavior.revoke_status = 200;
    });
    let signing_out = {
        let plan = rig.plan.clone();
        tokio::spawn(async move { plan.sign_out().await })
    };
    rig.fake.revoke_arrived().await;

    // Tokenless forget advances generation while the first revoke is still open.
    assert!(!rig.plan.forget().await.expect("forgets").revoked);

    rig.fake.set(|behavior| {
        behavior.hold = None;
        behavior.subject = "user-2".to_owned();
    });
    let callback = rig.start(Some("oaiapp_2")).await;
    rig.plan
        .complete_sign_in(callback)
        .await
        .expect("new sign-in activates");

    rig.fake.release();
    assert!(
        signing_out
            .await
            .expect("joins")
            .expect("signs out")
            .revoked
    );

    let status = rig.plan.status();
    assert_eq!(status.status, Some(Status::Active));
    assert!(
        !status.cleanup_unconfirmed,
        "a pending revoke that succeeds after tokenless forget and re-sign-in must not leave a false warning"
    );
    assert!(!rig.record().unconfirmed_cleanup);

    let Rig {
        fake, dir, plan, ..
    } = rig;
    drop(plan);
    let reopened = ChatGptPlan::open(dir.path(), fake.config()).expect("reopens");
    assert!(!reopened.status().cleanup_unconfirmed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_held_sign_out_that_fails_after_forget_and_re_sign_in_flags_and_retains_warning() {
    let rig = Rig::new().await;
    rig.sign_in().await;
    rig.fake.set(|behavior| {
        behavior.hold = Some(Held::Revoke);
        behavior.revoke_status = 503;
    });
    let failing_sign_out = {
        let plan = rig.plan.clone();
        tokio::spawn(async move { plan.sign_out().await })
    };
    rig.fake.revoke_arrived().await;
    assert!(!rig.plan.forget().await.expect("forgets").revoked);

    rig.fake.set(|behavior| {
        behavior.hold = None;
        behavior.subject = "user-2".to_owned();
    });
    let callback = rig.start(Some("oaiapp_2")).await;
    rig.plan
        .complete_sign_in(callback)
        .await
        .expect("second sign-in activates");
    assert!(
        !rig.plan.status().cleanup_unconfirmed,
        "still pending before the 503 revoke receipt arrives"
    );

    rig.fake.release();
    assert!(
        !failing_sign_out
            .await
            .expect("joins")
            .expect("signs out")
            .revoked
    );
    assert!(
        rig.plan.status().cleanup_unconfirmed,
        "failed held revoke flags cleanup_unconfirmed on the newer active session"
    );
    assert!(rig.record().unconfirmed_cleanup);

    // A subsequent held sign-out that succeeds (200) across tokenless forget +
    // new sign-in does NOT erase the independent earlier failure.
    rig.fake.set(|behavior| {
        behavior.hold = Some(Held::Revoke);
        behavior.revoke_status = 200;
    });
    let confirmed_later = {
        let plan = rig.plan.clone();
        tokio::spawn(async move { plan.sign_out().await })
    };
    rig.fake.revoke_arrived().await;
    assert!(!rig.plan.forget().await.expect("forgets").revoked);

    rig.fake.set(|behavior| {
        behavior.hold = None;
        behavior.subject = "user-3".to_owned();
    });
    let callback = rig.start(Some("oaiapp_3")).await;
    rig.plan
        .complete_sign_in(callback)
        .await
        .expect("third sign-in activates");
    rig.fake.release();
    assert!(
        confirmed_later
            .await
            .expect("joins")
            .expect("signs out")
            .revoked
    );
    assert!(
        rig.plan.status().cleanup_unconfirmed,
        "an independent earlier unconfirmed cleanup remains visible"
    );
    assert!(rig.record().unconfirmed_cleanup);

    let Rig {
        fake, dir, plan, ..
    } = rig;
    drop(plan);
    let reopened = ChatGptPlan::open(dir.path(), fake.config()).expect("reopens");
    assert!(reopened.status().cleanup_unconfirmed);
}
