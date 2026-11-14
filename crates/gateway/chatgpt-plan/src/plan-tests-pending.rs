//! Tests for what the status reports as a pending sign-in: from the start of
//! an attempt until that attempt's own work is finished. A sign-out, a reset,
//! or a newer attempt ends that at once, and an attempt whose callback never
//! arrives stops being pending when it expires.

use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sign_in_stays_pending_until_its_exchange_finishes() {
    let rig = Rig::new().await;
    let callback = rig.start(Some("oaiapp_1")).await;
    rig.fake
        .set(|behavior| behavior.hold = Some(Held::Exchange));
    let completing = {
        let plan = rig.plan.clone();
        tokio::spawn(async move { plan.complete_sign_in(callback).await })
    };
    rig.fake.exchange_arrived().await;

    // The callback claimed the attempt, and the issuer has not answered. A
    // client that polls the status must not read "signed out, nothing
    // pending" here: it would stop waiting for a sign-in that is about to
    // become active.
    let during = rig.plan.status();
    assert_ne!(during.status, Some(Status::Active));
    assert!(during.sign_in_pending, "the exchange is still open");

    rig.fake.release();
    completing.await.expect("joins").expect("signs in");
    let after = rig.plan.status();
    assert_eq!(after.status, Some(Status::Active));
    assert!(
        !after.sign_in_pending,
        "nothing is pending once it is active"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reset_during_the_exchange_stops_reporting_a_pending_sign_in() {
    let rig = Rig::new().await;
    let callback = rig.start(Some("oaiapp_1")).await;
    rig.fake
        .set(|behavior| behavior.hold = Some(Held::Exchange));
    let completing = {
        let plan = rig.plan.clone();
        tokio::spawn(async move { plan.complete_sign_in(callback).await })
    };
    rig.fake.exchange_arrived().await;
    assert!(rig.plan.status().sign_in_pending);

    rig.plan.reset().expect("resets");
    assert!(
        !rig.plan.status().sign_in_pending,
        "the reset ended the sign-in, though its exchange is still open"
    );
    rig.fake.release();
    assert!(completing.await.expect("joins").is_err());
    assert!(!rig.plan.status().sign_in_pending);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_newer_sign_in_that_finishes_first_is_not_masked_by_the_older_one() {
    let rig = Rig::new().await;
    let first = rig.start(Some("oaiapp_1")).await;
    rig.fake
        .set(|behavior| behavior.hold = Some(Held::Exchange));
    let completing = {
        let plan = rig.plan.clone();
        tokio::spawn(async move { plan.complete_sign_in(first).await })
    };
    rig.fake.exchange_arrived().await;

    // The first exchange stays open at the issuer. A second attempt starts
    // and completes while it does.
    rig.fake.set(|behavior| behavior.hold = None);
    let second = rig.start(None).await;
    rig.plan
        .complete_sign_in(second)
        .await
        .expect("the newer sign-in completes");
    let status = rig.plan.status();
    assert_eq!(status.status, Some(Status::Active));
    assert!(
        !status.sign_in_pending,
        "the older exchange is still open, and it is not this sign-in"
    );

    rig.fake.release();
    assert!(matches!(
        completing.await.expect("joins"),
        Err(PlanError::Superseded)
    ));
    let status = rig.plan.status();
    assert_eq!(status.status, Some(Status::Active));
    assert!(!status.sign_in_pending);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_newer_sign_in_that_is_refused_leaves_nothing_pending() {
    let rig = Rig::new().await;
    let first = rig.start(Some("oaiapp_1")).await;
    rig.fake
        .set(|behavior| behavior.hold = Some(Held::Exchange));
    let completing = {
        let plan = rig.plan.clone();
        tokio::spawn(async move { plan.complete_sign_in(first).await })
    };
    rig.fake.exchange_arrived().await;

    // A second attempt replaces the first, whose exchange is still open,
    // and the person refuses it in the browser.
    let mut refused = rig.start(None).await;
    refused.code = None;
    refused.error = Some("access_denied".to_owned());
    assert!(matches!(
        rig.plan.complete_sign_in(refused).await,
        Err(PlanError::Denied(_))
    ));
    assert!(
        !rig.plan.status().sign_in_pending,
        "the first attempt cannot activate, so its open exchange is not a pending sign-in"
    );

    rig.fake.release();
    assert!(completing.await.expect("joins").is_err());
    assert!(!rig.plan.status().sign_in_pending);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_older_completion_that_ends_late_does_not_clear_a_newer_ones_mark() {
    let rig = Rig::new().await;
    let inner = rig.plan.inner.clone();
    inner.lock().completing = Some(2);

    drop(crate::plan::sign_in::Completing(inner.clone(), 1));
    assert_eq!(
        inner.lock().completing,
        Some(2),
        "the older completion's mark is not the one that is set"
    );
    drop(crate::plan::sign_in::Completing(inner.clone(), 2));
    assert_eq!(inner.lock().completing, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_attempt_that_expires_unanswered_is_no_longer_pending() {
    let rig = Rig::new().await;
    let _unanswered = rig.start(Some("oaiapp_1")).await;
    assert!(rig.plan.status().sign_in_pending);

    // The browser never comes back, and the attempt's ten minutes pass.
    rig.advance(601);
    assert!(
        !rig.plan.status().sign_in_pending,
        "an attempt whose callback would be refused is not a pending sign-in"
    );

    // A new attempt is pending again.
    let _newer = rig.start(None).await;
    assert!(rig.plan.status().sign_in_pending);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_replaced_tokens_revocation_is_part_of_the_pending_sign_in() {
    let rig = Rig::new().await;
    rig.sign_in().await;
    rig.fake.set(|behavior| {
        behavior.hold = Some(Held::Revoke);
        behavior.revoke_status = 503;
    });
    let second = rig.start(None).await;
    let completing = {
        let plan = rig.plan.clone();
        tokio::spawn(async move { plan.complete_sign_in(second).await })
    };
    rig.fake.revoke_arrived().await;

    // The new sign-in is active, and the token it replaced is still being
    // revoked. The status stays pending, so a client that polls reads the
    // revocation's outcome in the first status that is not pending.
    let during = rig.plan.status();
    assert_eq!(during.status, Some(Status::Active));
    assert!(
        during.sign_in_pending,
        "the replaced token is not revoked yet"
    );
    assert!(!during.cleanup_unconfirmed);

    rig.fake.release();
    completing.await.expect("joins").expect("signs in");
    let after = rig.plan.status();
    assert_eq!(after.status, Some(Status::Active));
    assert!(!after.sign_in_pending);
    assert!(
        after.cleanup_unconfirmed,
        "the failed revocation is in the first status that is not pending"
    );
}
