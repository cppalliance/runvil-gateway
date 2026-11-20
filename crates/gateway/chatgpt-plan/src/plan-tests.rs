//! Tests for the broker, against the fake sign-in service: signing in and
//! out here, sign-in races in `plan-tests-races.rs`, refresh and its races
//! in `plan-tests-refresh.rs`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use super::*;
use crate::fake_issuer::{Fake, Held};
use crate::store::Credentials;

#[path = "plan-tests-failures.rs"]
mod failures;
#[path = "plan-tests-pending.rs"]
mod pending;
#[path = "plan-tests-races.rs"]
mod races;
#[path = "plan-tests-refresh.rs"]
mod refresh;

const REDIRECT: &str = "http://127.0.0.1:4567/auth/callback";

/// A broker over a private temp directory, a fake issuer, and a clock the
/// test moves.
struct Rig {
    fake: Fake,
    dir: tempfile::TempDir,
    plan: ChatGptPlan,
    now: Arc<AtomicU64>,
}

fn private_dir() -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().expect("tempdir");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .expect("chmod");
    }
    dir
}

impl Rig {
    /// A rig whose directory holds `seed` before the broker opens it.
    async fn seeded(seed: Option<Record>) -> Rig {
        let fake = Fake::start().await;
        let dir = private_dir();
        if let Some(record) = seed {
            // The fake honors the seeded refresh token, as if it issued it.
            if let Some(token) = record.session.refresh_token() {
                fake.accept_refresh_token(token.expose());
            }
            Store::open(dir.path())
                .expect("open")
                .save(&record)
                .expect("seed");
        }
        let real_now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_secs();
        let now = Arc::new(AtomicU64::new(real_now));
        let clock: Clock = {
            let now = now.clone();
            Arc::new(move || now.load(Ordering::SeqCst))
        };
        let plan = ChatGptPlan::open_with_clock(dir.path(), fake.config(), clock).expect("open");
        Rig {
            fake,
            dir,
            plan,
            now,
        }
    }

    async fn new() -> Rig {
        Rig::seeded(None).await
    }

    fn advance(&self, secs: u64) {
        self.now.fetch_add(secs, Ordering::SeqCst);
    }

    /// The record as it is on disk.
    fn record(&self) -> Record {
        self.plan
            .inner
            .store
            .load()
            .expect("loads")
            .expect("a record")
    }

    /// Starts a sign-in and returns the callback the issuer would send,
    /// teaching the fake the attempt's nonce on the way.
    async fn start(&self, issued_client: Option<&str>) -> Callback {
        let url = self.plan.begin_sign_in(REDIRECT).await.expect("begins");
        let query: HashMap<String, String> = url::Url::parse(&url)
            .expect("a url")
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        self.fake
            .set(|behavior| behavior.nonce.clone_from(&query["nonce"]));
        self.fake
            .expect_exchange(&query["code_challenge"], &query["redirect_uri"]);
        Callback {
            state: query["state"].clone(),
            code: Some("code-1".to_owned()),
            error: None,
            client_id: issued_client.map(str::to_owned),
        }
    }

    /// A complete first sign-in as `oaiapp_1`.
    async fn sign_in(&self) {
        let callback = self.start(Some("oaiapp_1")).await;
        self.plan
            .complete_sign_in(callback)
            .await
            .expect("signs in");
    }
}

/// An active record, as a previous run would have left it.
fn active() -> Record {
    Record {
        client_id: Some("oaiapp_1".to_owned()),
        subject: Some("user-1".to_owned()),
        email: Some("a@example.test".to_owned()),
        session: Session::Active(Credentials {
            refresh_token: Secret::new("rt-seed".to_owned()),
            scopes: crate::oauth::SCOPE.split(' ').map(str::to_owned).collect(),
        }),
        ..Record::new("urn:uuid:11111111-1111-4111-8111-111111111111".to_owned())
    }
}

#[tokio::test]
async fn a_first_sign_in_registers_activates_and_saves_no_access_token() {
    let rig = Rig::new().await;
    assert_eq!(rig.plan.status().status, None);
    rig.sign_in().await;

    let status = rig.plan.status();
    assert_eq!(status.status, Some(Status::Active));
    assert_eq!(status.email.as_deref(), Some("a@example.test"));
    assert!(status.plan_granted);
    assert!(!status.sign_in_pending);

    let record = rig.record();
    assert_eq!(record.client_id.as_deref(), Some("oaiapp_1"));
    assert_eq!(record.subject.as_deref(), Some("user-1"));
    assert_eq!(
        record.session.refresh_token().expect("kept").expose(),
        "rt-1"
    );
    // urn:uuid: plus a version-4 UUID in the 8-4-4-4-12 form.
    let uuid = record
        .agent_host_id
        .strip_prefix("urn:uuid:")
        .expect("a urn");
    let groups: Vec<usize> = uuid.split('-').map(str::len).collect();
    assert_eq!(groups, [8, 4, 4, 4, 12]);
    assert_eq!(uuid.as_bytes()[14], b'4', "version 4");
    let on_disk = std::fs::read_to_string(
        rig.dir
            .path()
            .join(crate::store::STORE_DIR)
            .join(crate::store::RECORD_FILE),
    )
    .expect("read");
    assert!(
        !on_disk.contains("at-1"),
        "the access token is never written"
    );

    // The exchange used the issued client, and the cached token serves.
    assert_eq!(
        rig.fake
            .seen(|seen| seen.token_requests[0]["client_id"].clone()),
        "oaiapp_1"
    );
    assert_eq!(
        rig.plan.access_token().await.expect("a token").expose(),
        "at-1"
    );
    assert_eq!(rig.fake.grants(), ["authorization_code"]);
}

#[tokio::test]
async fn signing_in_again_while_signed_in_revokes_the_token_it_replaces() {
    let rig = Rig::new().await;
    rig.sign_in().await;
    assert_eq!(
        rig.record().session.refresh_token().expect("held").expose(),
        "rt-1"
    );

    // No sign-out in between: the second sign-in replaces rt-1.
    let callback = rig.start(None).await;
    rig.plan
        .complete_sign_in(callback)
        .await
        .expect("signs in again");
    assert_eq!(
        rig.record().session.refresh_token().expect("held").expose(),
        "rt-2"
    );
    assert_eq!(rig.plan.status().status, Some(Status::Active));
    assert_eq!(
        rig.fake.seen(|seen| seen.revoked.clone()),
        ["rt-1"],
        "the replaced token is revoked, not left live"
    );
}

#[tokio::test]
async fn a_wrong_state_consumes_nothing_and_a_state_works_once() {
    let rig = Rig::new().await;
    let callback = rig.start(Some("oaiapp_1")).await;
    let forged = Callback {
        state: "not-the-state".to_owned(),
        ..callback.clone()
    };
    assert!(matches!(
        rig.plan.complete_sign_in(forged).await,
        Err(PlanError::UnknownAttempt)
    ));
    assert!(
        rig.fake.grants().is_empty(),
        "a forged callback sends no token request"
    );
    assert!(
        rig.plan.status().sign_in_pending,
        "the real attempt is still waiting"
    );

    rig.plan
        .complete_sign_in(callback.clone())
        .await
        .expect("the real callback works");
    assert!(matches!(
        rig.plan.complete_sign_in(callback).await,
        Err(PlanError::UnknownAttempt)
    ));
    assert_eq!(
        rig.fake.grants().len(),
        1,
        "a replayed callback sends nothing"
    );
}

#[tokio::test]
async fn a_denial_sends_no_token_request() {
    let rig = Rig::new().await;
    let mut callback = rig.start(Some("oaiapp_1")).await;
    callback.code = None;
    callback.error = Some("access_denied".to_owned());
    match rig.plan.complete_sign_in(callback).await {
        Err(PlanError::Denied(code)) => assert_eq!(code, "access_denied"),
        other => panic!("expected a denial, got {other:?}"),
    }
    assert!(rig.fake.grants().is_empty());
    assert!(!rig.plan.status().sign_in_pending);

    let mut callback = rig.start(Some("oaiapp_1")).await;
    callback.error = Some("<script>alert(1)</script>".to_owned());
    match rig.plan.complete_sign_in(callback).await {
        Err(PlanError::Denied(code)) => assert_eq!(code, "unrecognized_error"),
        other => panic!("expected a denial, got {other:?}"),
    }
}

#[tokio::test]
async fn an_expired_attempt_is_refused_without_a_token_request() {
    let rig = Rig::new().await;
    let callback = rig.start(Some("oaiapp_1")).await;
    rig.advance(601);
    assert!(matches!(
        rig.plan.complete_sign_in(callback).await,
        Err(PlanError::Superseded)
    ));
    assert!(rig.fake.grants().is_empty());
}

#[tokio::test]
async fn a_first_sign_in_without_an_issued_client_is_incomplete() {
    let rig = Rig::new().await;
    let callback = rig.start(None).await;
    assert!(matches!(
        rig.plan.complete_sign_in(callback).await,
        Err(PlanError::Incomplete(_))
    ));
    assert!(rig.fake.grants().is_empty());
}

#[tokio::test]
async fn a_missing_id_token_fails_closed_and_revokes_what_was_issued() {
    let rig = Rig::new().await;
    let callback = rig.start(Some("oaiapp_1")).await;
    rig.fake.set(|behavior| behavior.omit_id_token = true);
    assert!(matches!(
        rig.plan.complete_sign_in(callback).await,
        Err(PlanError::Incomplete(_))
    ));
    assert_eq!(rig.plan.status().status, Some(Status::SignedOut));
    assert_eq!(rig.fake.seen(|seen| seen.revoked.clone()), ["rt-1"]);
    assert!(rig.record().session.refresh_token().is_none());
}

#[tokio::test]
async fn a_wrong_nonce_is_refused_and_the_tokens_revoked() {
    let rig = Rig::new().await;
    let callback = rig.start(Some("oaiapp_1")).await;
    rig.fake
        .set(|behavior| behavior.nonce = "another-attempts-nonce".to_owned());
    assert!(matches!(
        rig.plan.complete_sign_in(callback).await,
        Err(PlanError::IdToken(IdTokenError::Nonce))
    ));
    assert_eq!(rig.fake.seen(|seen| seen.revoked.clone()), ["rt-1"]);
    assert_eq!(rig.plan.status().status, Some(Status::SignedOut));
}

#[tokio::test]
async fn signing_in_again_needs_no_client_id_and_refuses_a_different_one() {
    let rig = Rig::new().await;
    rig.sign_in().await;
    rig.plan.sign_out().await.expect("signs out");

    // A later callback carries no client id: that is success.
    let callback = rig.start(None).await;
    rig.plan
        .complete_sign_in(callback)
        .await
        .expect("signs in again");
    assert_eq!(rig.plan.status().status, Some(Status::Active));
    assert_eq!(
        rig.fake
            .seen(|seen| seen.token_requests[1]["client_id"].clone()),
        "oaiapp_1"
    );

    // One naming another registration is refused before any exchange.
    let before = rig.fake.grants().len();
    let callback = rig.start(Some("oaiapp_other")).await;
    assert!(matches!(
        rig.plan.complete_sign_in(callback).await,
        Err(PlanError::ClientMismatch)
    ));
    assert_eq!(rig.fake.grants().len(), before);
}

#[tokio::test]
async fn another_account_cannot_take_over_the_registration() {
    let rig = Rig::new().await;
    rig.sign_in().await;
    rig.plan.sign_out().await.expect("signs out");

    let callback = rig.start(None).await;
    rig.fake
        .set(|behavior| behavior.subject = "user-2".to_owned());
    assert!(matches!(
        rig.plan.complete_sign_in(callback).await,
        Err(PlanError::AccountMismatch)
    ));
    assert_eq!(rig.plan.status().status, Some(Status::SignedOut));
    assert_eq!(rig.record().subject.as_deref(), Some("user-1"));
    // rt-1 by the sign-out, rt-2 because the mismatched sign-in's token was
    // not kept.
    assert_eq!(rig.fake.seen(|seen| seen.revoked.clone()), ["rt-1", "rt-2"]);
}

#[tokio::test]
async fn sign_out_is_local_first_and_reports_the_remote_outcome_honestly() {
    let rig = Rig::new().await;
    rig.sign_in().await;
    assert_eq!(
        rig.plan.sign_out().await.expect("signs out"),
        SignedOut { revoked: true }
    );
    let record = rig.record();
    assert_eq!(
        record.session,
        Session::SignedOut {
            revocation: Some(Revocation::Confirmed),
        }
    );
    assert!(!rig.plan.status().plan_granted);
    assert_eq!(
        record.client_id.as_deref(),
        Some("oaiapp_1"),
        "the registration is kept"
    );
    assert!(matches!(
        rig.plan.access_token().await,
        Err(PlanError::NotSignedIn)
    ));

    // A revoke the issuer does not confirm is reported as unconfirmed.
    let callback = rig.start(None).await;
    rig.plan
        .complete_sign_in(callback)
        .await
        .expect("signs in again");
    rig.fake.set(|behavior| behavior.revoke_status = 500);
    assert_eq!(
        rig.plan.sign_out().await.expect("signs out"),
        SignedOut { revoked: false }
    );
    let record = rig.record();
    assert_eq!(
        record.session,
        Session::SignedOut {
            revocation: Some(Revocation::Unconfirmed),
        },
        "signed out here even so"
    );
}

#[tokio::test]
async fn forget_keeps_only_the_agent_host_id_and_reset_clears_an_unreadable_record() {
    let rig = Rig::new().await;
    rig.sign_in().await;
    let agent_host_id = rig.record().agent_host_id;
    rig.plan.forget().await.expect("forgets");
    let record = rig.record();
    assert_eq!(record.agent_host_id, agent_host_id);
    assert!(record.client_id.is_none() && record.subject.is_none() && record.email.is_none());
    assert_eq!(
        record.session,
        Session::SignedOut {
            revocation: Some(Revocation::Confirmed),
        }
    );

    // An unreadable record refuses everything until it is reset.
    let dir = private_dir();
    Store::open(dir.path()).expect("open");
    let record_path = dir
        .path()
        .join(crate::store::STORE_DIR)
        .join(crate::store::RECORD_FILE);
    std::fs::write(&record_path, b"{ not json").expect("write");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&record_path, std::fs::Permissions::from_mode(0o600))
            .expect("chmod");
    }
    let fake = Fake::start().await;
    let plan = ChatGptPlan::open(dir.path(), fake.config()).expect("open");
    assert!(plan.status().unreadable);
    assert!(matches!(
        plan.access_token().await,
        Err(PlanError::Unreadable)
    ));
    assert!(matches!(
        plan.begin_sign_in(REDIRECT).await,
        Err(PlanError::Unreadable)
    ));
    plan.reset().expect("resets");
    assert!(!plan.status().unreadable);
    plan.begin_sign_in(REDIRECT)
        .await
        .expect("a fresh install can sign in");
    tokio::time::sleep(Duration::ZERO).await;
}
