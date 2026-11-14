//! Tests for refresh: sharing, failure classes, restarts, and the races
//! with sign-out and with a caller that goes away. The race tests hold the
//! refresh request open at the fake, so the order is forced, not hoped for.

use std::time::Duration;

use super::*;

fn refresh_count(rig: &Rig) -> usize {
    rig.fake
        .grants()
        .iter()
        .filter(|grant| *grant == "refresh_token")
        .count()
}

/// Waits until the record on disk satisfies `ready`.
async fn until(rig: &Rig, ready: impl Fn(&Record) -> bool) -> Record {
    for _ in 0..200 {
        let record = rig.record();
        if ready(&record) {
            return record;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "the record never reached the expected state: {:?}",
        rig.record()
    );
}

#[tokio::test]
async fn a_restart_refreshes_once_and_serves_the_cached_token_after() {
    let rig = Rig::seeded(Some(active())).await;
    assert_eq!(
        rig.plan.access_token().await.expect("a token").expose(),
        "at-1"
    );
    assert_eq!(
        rig.plan.access_token().await.expect("a token").expose(),
        "at-1"
    );
    assert_eq!(refresh_count(&rig), 1);
    let record = rig.record();
    assert_eq!(
        record.session.refresh_token().expect("rotated").expose(),
        "rt-1"
    );
    assert!(
        matches!(record.session, Session::Active(_)),
        "the marker is cleared once the result is saved"
    );

    // Within two minutes of expiry the token is refreshed again.
    rig.advance(3600 - 119);
    assert_eq!(
        rig.plan.access_token().await.expect("a token").expose(),
        "at-2"
    );
    assert_eq!(refresh_count(&rig), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_callers_share_one_refresh() {
    let rig = Rig::seeded(Some(active())).await;
    rig.fake.set(|behavior| behavior.hold = Some(Held::Refresh));
    let callers: Vec<_> = (0..8)
        .map(|_| {
            let plan = rig.plan.clone();
            tokio::spawn(async move { plan.access_token().await })
        })
        .collect();
    rig.fake.refresh_arrived().await;
    // The marker is on disk while the request is in flight.
    assert!(matches!(rig.record().session, Session::Refreshing(_)));
    rig.fake.release();
    for caller in callers {
        assert_eq!(
            caller.await.expect("joins").expect("a token").expose(),
            "at-1"
        );
    }
    assert_eq!(refresh_count(&rig), 1, "eight callers, one refresh");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sign_out_during_a_refresh_wins_and_the_late_token_is_revoked() {
    let rig = Rig::seeded(Some(active())).await;
    rig.fake.set(|behavior| behavior.hold = Some(Held::Refresh));
    let caller = {
        let plan = rig.plan.clone();
        tokio::spawn(async move { plan.access_token().await })
    };
    rig.fake.refresh_arrived().await;

    // The refresh request is open at the issuer. Sign out now.
    assert!(rig.plan.sign_out().await.expect("signs out").revoked);
    assert_eq!(rig.record().session.status(), Status::SignedOut);

    rig.fake.release();
    assert!(matches!(
        caller.await.expect("joins"),
        Err(PlanError::Superseded)
    ));
    // rt-seed by the sign-out; rt-1 is the refresh's late reply, revoked
    // rather than saved.
    assert_eq!(
        rig.fake.seen(|seen| seen.revoked.clone()),
        ["rt-seed", "rt-1"]
    );
    let record = rig.record();
    assert_eq!(record.session.status(), Status::SignedOut);
    assert!(
        record.session.refresh_token().is_none(),
        "the late token was not resurrected"
    );
    assert!(matches!(
        rig.plan.access_token().await,
        Err(PlanError::NotSignedIn)
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_caller_that_goes_away_does_not_abandon_its_refresh() {
    let rig = Rig::seeded(Some(active())).await;
    rig.fake.set(|behavior| behavior.hold = Some(Held::Refresh));
    let caller = {
        let plan = rig.plan.clone();
        tokio::spawn(async move { plan.access_token().await })
    };
    rig.fake.refresh_arrived().await;
    caller.abort();
    let _ = caller.await;

    rig.fake.release();
    // The broker's own task finishes the refresh and saves the rotation.
    let record = until(&rig, |record| {
        !matches!(record.session, Session::Refreshing(_))
    })
    .await;
    assert_eq!(
        record.session.refresh_token().expect("rotated").expose(),
        "rt-1"
    );
    assert!(matches!(record.session, Session::Active(_)));
    assert_eq!(
        rig.plan.access_token().await.expect("cached").expose(),
        "at-1"
    );
    assert_eq!(refresh_count(&rig), 1);
}

#[tokio::test]
async fn a_dead_refresh_token_needs_a_new_sign_in_and_keeps_the_registration() {
    let rig = Rig::seeded(Some(active())).await;
    rig.fake
        .set(|behavior| behavior.token_error = Some((400, "invalid_grant")));
    assert!(matches!(
        rig.plan.access_token().await,
        Err(PlanError::NeedsAuth)
    ));
    let record = rig.record();
    assert_eq!(
        record.session,
        Session::NeedsAuth {
            refresh_token: None
        }
    );
    assert_eq!(record.client_id.as_deref(), Some("oaiapp_1"));
    // Nothing is retried.
    assert!(matches!(
        rig.plan.access_token().await,
        Err(PlanError::NeedsAuth)
    ));
    assert_eq!(refresh_count(&rig), 1);
}

#[tokio::test]
async fn a_passing_refusal_changes_nothing() {
    let rig = Rig::seeded(Some(active())).await;
    rig.fake
        .set(|behavior| behavior.token_error = Some((503, "server_error")));
    assert!(matches!(
        rig.plan.access_token().await,
        Err(PlanError::OAuth(OAuthError::Refused { status: 503, .. }))
    ));
    let record = rig.record();
    assert!(matches!(record.session, Session::Active(_)));
    assert_eq!(
        record.session.refresh_token().expect("kept").expose(),
        "rt-seed"
    );

    rig.fake.set(|behavior| behavior.token_error = None);
    assert_eq!(
        rig.plan.access_token().await.expect("recovers").expose(),
        "at-1"
    );
}

#[tokio::test]
async fn an_unreachable_issuer_does_not_sign_the_user_out() {
    // The issuer's port is closed: the request never leaves.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let issuer = format!("http://{}", listener.local_addr().expect("addr"));
    drop(listener);
    let dir = private_dir();
    Store::open(dir.path())
        .expect("open")
        .save(&active())
        .expect("seed");
    let plan = ChatGptPlan::open(
        dir.path(),
        OAuthConfig {
            issuer,
            resource: "https://api.example.test/v1".to_owned(),
            agent_name: "Runvil".to_owned(),
        },
    )
    .expect("open");
    assert!(matches!(
        plan.access_token().await,
        Err(PlanError::OAuth(OAuthError::Connect))
    ));
    let record = plan.inner.store.load().expect("loads").expect("a record");
    assert!(matches!(record.session, Session::Active(_)));
    assert_eq!(
        record.session.refresh_token().expect("kept").expose(),
        "rt-seed"
    );
}

#[tokio::test]
async fn a_refresh_with_no_reply_has_an_unknown_outcome_and_fails_closed() {
    // An issuer that accepts every connection and closes it without a word.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let issuer = format!("http://{}", listener.local_addr().expect("addr"));
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let mut buffer = [0_u8; 4096];
            let _ = tokio::io::AsyncReadExt::read(&mut stream, &mut buffer).await;
        }
    });
    let dir = private_dir();
    Store::open(dir.path())
        .expect("open")
        .save(&active())
        .expect("seed");
    let plan = ChatGptPlan::open(
        dir.path(),
        OAuthConfig {
            issuer,
            resource: "https://api.example.test/v1".to_owned(),
            agent_name: "Runvil".to_owned(),
        },
    )
    .expect("open");
    assert!(matches!(
        plan.access_token().await,
        Err(PlanError::NeedsAuth)
    ));
    let record = plan.inner.store.load().expect("loads").expect("a record");
    assert_eq!(
        record.session,
        Session::NeedsAuth {
            refresh_token: Some(Secret::new("rt-seed".to_owned())),
        },
        "the refresh token may be spent"
    );
}

#[tokio::test]
async fn a_restart_that_finds_a_refresh_in_flight_needs_a_new_sign_in() {
    let base = active();
    let Session::Active(creds) = base.session else {
        unreachable!()
    };
    let interrupted = Record {
        session: Session::Refreshing(creds),
        ..base
    };
    let rig = Rig::seeded(Some(interrupted)).await;
    assert_eq!(rig.plan.status().status, Some(Status::NeedsAuth));
    assert!(matches!(
        rig.plan.access_token().await,
        Err(PlanError::NeedsAuth)
    ));
    assert_eq!(refresh_count(&rig), 0, "nothing is retried");
    let record = rig.record();
    assert_eq!(
        record.session,
        Session::NeedsAuth {
            refresh_token: Some(Secret::new("rt-seed".to_owned())),
        }
    );

    // Signing out still revokes the possibly-unspent token and leaves none on disk.
    assert!(rig.plan.sign_out().await.expect("signs out").revoked);
    assert_eq!(rig.fake.seen(|seen| seen.revoked.clone()), ["rt-seed"]);
    assert_eq!(
        rig.record().session,
        Session::SignedOut {
            revocation: Some(Revocation::Confirmed),
        }
    );
}

#[tokio::test]
async fn a_refreshed_id_token_for_another_account_ends_the_session() {
    let rig = Rig::seeded(Some(active())).await;
    rig.fake
        .set(|behavior| behavior.subject = "user-2".to_owned());
    assert!(matches!(
        rig.plan.access_token().await,
        Err(PlanError::NeedsAuth)
    ));
    let record = rig.record();
    assert_eq!(
        record.session,
        Session::NeedsAuth {
            refresh_token: None
        }
    );
    assert_eq!(
        rig.fake.seen(|seen| seen.revoked.clone()),
        ["rt-1"],
        "the new token is revoked"
    );
}

#[tokio::test]
async fn a_refresh_without_an_id_token_is_accepted() {
    let rig = Rig::seeded(Some(active())).await;
    rig.fake
        .set(|behavior| behavior.omit_id_token_on_refresh = true);
    assert_eq!(
        rig.plan.access_token().await.expect("a token").expose(),
        "at-1"
    );
    assert_eq!(rig.fake.seen(|seen| seen.jwks_hits), 0, "nothing to verify");
    assert!(rig.fake.seen(|seen| seen.revoked.is_empty()));
}

#[tokio::test]
async fn a_rotated_signing_key_is_fetched_once_and_then_trusted() {
    let rig = Rig::seeded(Some(active())).await;
    assert_eq!(
        rig.plan.access_token().await.expect("a token").expose(),
        "at-1"
    );
    assert_eq!(rig.fake.seen(|seen| seen.jwks_hits), 1);

    // The issuer rotates its signing key. The cached keys do not hold the
    // new id, so they are fetched again, once.
    rig.fake.set(|behavior| behavior.kid = "k2".to_owned());
    rig.advance(3600);
    assert_eq!(
        rig.plan.access_token().await.expect("a token").expose(),
        "at-2"
    );
    assert_eq!(rig.fake.seen(|seen| seen.jwks_hits), 2);
    rig.advance(3600);
    assert_eq!(
        rig.plan.access_token().await.expect("a token").expose(),
        "at-3"
    );
    assert_eq!(
        rig.fake.seen(|seen| seen.jwks_hits),
        2,
        "the new key is cached"
    );
}

#[tokio::test]
async fn a_rejected_access_token_is_dropped_so_the_next_request_refreshes() {
    let rig = Rig::seeded(Some(active())).await;
    let first = rig.plan.bearer().await.expect("a bearer");
    assert_eq!(first.expose(), "at-1");
    // A stale rejection (an older token) changes nothing.
    rig.plan
        .rejected(&gateway_config::Secret::new("at-0".to_owned()))
        .await;
    assert_eq!(rig.plan.bearer().await.expect("cached").expose(), "at-1");
    rig.plan.rejected(&first).await;
    assert_eq!(rig.plan.bearer().await.expect("refreshed").expose(), "at-2");
}

#[tokio::test]
async fn without_the_plan_scope_no_bearer_is_handed_out() {
    let mut record = active();
    if let Session::Active(creds) = &mut record.session {
        creds.scopes = vec!["openid".to_owned()];
    }
    let rig = Rig::seeded(Some(record)).await;
    match rig.plan.bearer().await {
        Err(ProtocolError::CredentialUnavailable { code }) => {
            assert_eq!(code, "chatgpt_plan_not_granted");
        }
        other => panic!("expected no credential, got {other:?}"),
    }
    assert_eq!(refresh_count(&rig), 0);
    assert!(!rig.plan.status().plan_granted);
}

#[tokio::test]
async fn a_signed_out_broker_reports_a_stable_code_and_never_a_token() {
    let rig = Rig::new().await;
    match rig.plan.bearer().await {
        Err(ProtocolError::CredentialUnavailable { code }) => {
            assert_eq!(code, "chatgpt_sign_in_required");
        }
        other => panic!("expected no credential, got {other:?}"),
    }
}

#[tokio::test]
async fn a_refresh_that_names_no_scope_keeps_the_granted_scopes() {
    let rig = Rig::seeded(Some(active())).await;
    rig.fake
        .set(|behavior| behavior.omit_scope_on_refresh = true);
    assert_eq!(
        rig.plan.access_token().await.expect("a token").expose(),
        "at-1"
    );
    assert!(rig.plan.status().plan_granted);
    assert_eq!(rig.record().session.scopes(), active().session.scopes());
}

#[tokio::test]
async fn a_refresh_that_drops_the_plan_scope_is_enforced() {
    let rig = Rig::seeded(Some(active())).await;
    rig.fake
        .set(|behavior| behavior.scope = "openid profile".to_owned());
    assert!(matches!(
        rig.plan.access_token().await,
        Err(PlanError::PlanNotGranted)
    ));
    assert_eq!(rig.record().session.scopes(), ["openid", "profile"]);
    assert!(!rig.plan.status().plan_granted);
    // The rotation itself was saved: the session is intact, only unusable
    // for the plan.
    assert_eq!(
        rig.record()
            .session
            .refresh_token()
            .expect("rotated")
            .expose(),
        "rt-1"
    );
}

#[tokio::test]
async fn an_issuer_fault_is_an_upstream_error_and_not_a_missing_credential() {
    let rig = Rig::seeded(Some(active())).await;
    rig.fake
        .set(|behavior| behavior.token_error = Some((503, "server_error")));
    assert!(
        matches!(
            rig.plan.bearer().await,
            Err(ProtocolError::UpstreamTransport(_))
        ),
        "a failing issuer is a 502-class fault"
    );

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let issuer = format!("http://{}", listener.local_addr().expect("addr"));
    drop(listener);
    let dir = private_dir();
    Store::open(dir.path())
        .expect("open")
        .save(&active())
        .expect("seed");
    let plan = ChatGptPlan::open(
        dir.path(),
        OAuthConfig {
            issuer,
            resource: "https://api.example.test/v1".to_owned(),
            agent_name: "Runvil".to_owned(),
        },
    )
    .expect("open");
    assert!(matches!(
        plan.bearer().await,
        Err(ProtocolError::UpstreamConnect(_))
    ));
}
