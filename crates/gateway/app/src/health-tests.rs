//! The health reply serializes as the literal it replaced.

use super::HealthReply;

#[test]
fn the_body_matches_the_literal_it_replaced() {
    let reply = HealthReply { status: "serving" };
    assert_eq!(
        serde_json::to_string(&reply).expect("the reply serializes"),
        serde_json::to_string(&serde_json::json!({ "status": "serving" }))
            .expect("the literal serializes")
    );
}
