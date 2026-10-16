//! Instance-local, one-use write capabilities. Never persist these or bearer tokens.
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};
const PREPARED_TTL: Duration = Duration::from_secs(10);
const RECEIPT_TTL: Duration = Duration::from_secs(120);
const LIMIT: usize = 32;
struct Operation {
    principal: String,
    capability: Option<String>,
    issued: Instant,
    entry_id: String,
    session_key: String,
    job: Value,
    receipt: Value,
    completed: Option<Instant>,
}
#[derive(Default)]
pub(crate) struct Actions {
    operations: BTreeMap<String, Operation>,
}
fn error(code: u16, message: &str) -> (u16, Value) {
    (code, json!({"error":message}))
}
impl Actions {
    fn prune(&mut self) {
        self.operations.retain(|_, op| {
            if op.receipt["state"] == "prepared" {
                op.issued.elapsed() < PREPARED_TTL
            } else {
                op.completed.is_none_or(|at| at.elapsed() < RECEIPT_TTL)
            }
        });
    }
    pub(crate) fn pending(&self) -> bool {
        self.operations
            .values()
            .any(|op| op.receipt["state"] == "pending")
    }
    pub(crate) fn prepare(
        &mut self,
        principal: &str,
        entry_id: &str,
        session_key: &str,
        target: Value,
        label: &str,
        requested: bool,
    ) -> (u16, Value) {
        self.prune();
        if self.operations.len() >= LIMIT || self.pending() {
            return error(
                409,
                "An operation is pending or the receipt limit was reached",
            );
        }
        let (Ok(id), Ok(capability)) = (
            crate::service::generate_token(),
            crate::service::generate_token(),
        ) else {
            return error(503, "Secure action identifiers are unavailable");
        };
        let job = json!({"operation_id":id,"requested_muted":requested,"target":target});
        let receipt = json!({"operation_id":id,"state":"prepared","requested_muted":requested,"observed_muted":null,"readback_age_ms":null,"error":null,"target":{"entry_id":entry_id,"label":label,"process_id":job["target"]["process_id"]}});
        let mut public = receipt.clone();
        public["capability"] = json!(capability);
        public["expires_in_ms"] = json!(PREPARED_TTL.as_millis());
        self.operations.insert(
            id,
            Operation {
                principal: principal.into(),
                capability: Some(capability),
                issued: Instant::now(),
                entry_id: entry_id.into(),
                session_key: session_key.into(),
                job,
                receipt,
                completed: None,
            },
        );
        (200, public)
    }
    /// Consume before any dispatch. Replays cannot enqueue another write.
    pub(crate) fn consume(
        &mut self,
        principal: &str,
        body: &str,
    ) -> Result<(String, String, String, Value), (u16, Value)> {
        self.prune();
        let value: Value =
            serde_json::from_str(body).map_err(|_| error(400, "Expected a JSON object"))?;
        let object = value
            .as_object()
            .ok_or_else(|| error(400, "Expected a JSON object"))?;
        if object.len() != 2
            || object
                .keys()
                .any(|k| !matches!(k.as_str(), "operation_id" | "capability"))
        {
            return Err(error(400, "Unexpected action field"));
        }
        let id = value["operation_id"]
            .as_str()
            .ok_or_else(|| error(400, "Operation id is required"))?;
        let cap = value["capability"]
            .as_str()
            .ok_or_else(|| error(400, "Capability is required"))?;
        let pending = self.pending();
        let Some(op) = self
            .operations
            .get_mut(id)
            .filter(|op| op.principal == principal)
        else {
            return Err(error(
                404,
                "Operation is unavailable; no successful outcome is established",
            ));
        };
        if op.capability.as_deref() != Some(cap) || op.receipt["state"] != "prepared" {
            return Err(error(409, "Capability expired or was already consumed"));
        }
        op.capability = None;
        if pending {
            op.receipt["state"] = json!("rejected_before_write");
            op.receipt["error"] = json!("another_operation_pending");
            op.completed = Some(Instant::now());
            return Err((409, op.receipt.clone()));
        }
        op.receipt["state"] = json!("pending");
        Ok((
            id.into(),
            op.entry_id.clone(),
            op.session_key.clone(),
            op.job.clone(),
        ))
    }
    pub(crate) fn reject(&mut self, id: &str, reason: &str) {
        if let Some(op) = self.operations.get_mut(id) {
            op.receipt["state"] = json!("rejected_before_write");
            op.receipt["error"] = json!(reason);
            op.completed = Some(Instant::now());
        }
    }
    pub(crate) fn complete(&mut self, id: &str, result: Value, observed_since: Instant) {
        let Some(op) = self
            .operations
            .get_mut(id)
            .filter(|op| op.receipt["state"] == "pending")
        else {
            return;
        };
        // Never trust malformed, mismatched or optimistic native output.
        let state = result["state"].as_str().unwrap_or("");
        let observed = result["observed_muted"].as_bool();
        let requested = op.job["requested_muted"].as_bool().unwrap();
        let verified = (state == "verified" && observed == Some(requested))
            || (state == "verified_mismatch" && observed == Some(!requested));
        let failure = matches!(state, "rejected_before_write" | "outcome_unconfirmed")
            && result["observed_muted"].is_null();
        let known_error = result["error"].is_null()
            || result["error"].as_str().is_some_and(|s| {
                s.len() <= 100 && s.bytes().all(|c| c.is_ascii_lowercase() || c == b'_')
            });
        let valid = result["operation_id"] == id
            && result["requested_muted"] == requested
            && (verified || failure)
            && known_error;
        op.receipt["state"] = json!(if valid { state } else { "outcome_unconfirmed" });
        op.receipt["observed_muted"] = if valid {
            result["observed_muted"].clone()
        } else {
            Value::Null
        };
        op.receipt["error"] = if valid {
            result["error"].clone()
        } else {
            json!("invalid_native_receipt")
        };
        op.completed = Some(observed_since);
    }
    pub(crate) fn owner_stopped(&mut self) {
        for op in self
            .operations
            .values_mut()
            .filter(|op| op.receipt["state"] == "pending")
        {
            op.receipt["state"] = json!("outcome_unconfirmed");
            op.receipt["error"] = json!("action_owner_stopped");
            op.completed = Some(Instant::now());
        }
    }
    pub(crate) fn view(&mut self, principal: &str, id: &str) -> (u16, Value) {
        self.prune();
        let Some(op) = self
            .operations
            .get(id)
            .filter(|op| op.principal == principal)
        else {
            return error(
                404,
                "Operation is unavailable; no successful outcome is established",
            );
        };
        let mut result = op.receipt.clone();
        if result["observed_muted"].is_boolean() {
            result["readback_age_ms"] =
                json!(op
                    .completed
                    .map(|t| t.elapsed().as_millis().min(u64::MAX as u128) as u64));
        }
        (200, result)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn prepare(actions: &mut Actions) -> Value {
        actions
            .prepare(
                "browser-a",
                "entry",
                "key",
                json!({"process_id":42}),
                "Fixture",
                true,
            )
            .1
    }
    fn body(p: &Value) -> String {
        json!({"operation_id":p["operation_id"],"capability":p["capability"]}).to_string()
    }
    #[test]
    fn capabilities_bind_principal_and_are_one_use() {
        let mut a = Actions::default();
        let p = prepare(&mut a);
        assert_eq!(a.consume("browser-b", &body(&p)).unwrap_err().0, 404);
        let (_, _, _, job) = a.consume("browser-a", &body(&p)).unwrap();
        assert_eq!(job["requested_muted"], true);
        assert_eq!(a.consume("browser-a", &body(&p)).unwrap_err().0, 409);
        assert_eq!(
            a.view("browser-b", p["operation_id"].as_str().unwrap()).0,
            404
        );
        assert!(!a
            .view("browser-a", p["operation_id"].as_str().unwrap())
            .1
            .to_string()
            .contains("capability"));
    }
    #[test]
    fn preparation_never_dispatches_and_pending_blocks_new_actions() {
        let mut a = Actions::default();
        let p = prepare(&mut a);
        let second = prepare(&mut a);
        assert!(!a.pending());
        a.consume("browser-a", &body(&p)).unwrap();
        assert!(a.pending());
        assert_eq!(
            a.consume("browser-a", &body(&second)).unwrap_err().1["state"],
            "rejected_before_write"
        );
        assert_eq!(
            a.prepare("browser-a", "entry", "key", json!({}), "Fixture", false)
                .0,
            409
        );
    }
    #[test]
    fn expired_or_extra_fields_fail_closed() {
        let mut a = Actions::default();
        let p = prepare(&mut a);
        let id = p["operation_id"].as_str().unwrap();
        let mut malformed: Value = serde_json::from_str(&body(&p)).unwrap();
        malformed["requested_muted"] = json!(false);
        assert_eq!(
            a.consume("browser-a", &malformed.to_string())
                .unwrap_err()
                .0,
            400
        );
        a.operations.get_mut(id).unwrap().issued = Instant::now() - Duration::from_secs(11);
        assert_eq!(a.consume("browser-a", &body(&p)).unwrap_err().0, 404);
    }
    #[test]
    fn missing_or_lying_readback_is_not_success() {
        for result in [
            json!({"state":"verified","observed_muted":null}),
            json!({"state":"verified","observed_muted":false}),
            json!({"state":"verified","observed_muted":true,"error":"Private/Path"}),
        ] {
            let mut a = Actions::default();
            let p = prepare(&mut a);
            let id = p["operation_id"].as_str().unwrap();
            a.consume("browser-a", &body(&p)).unwrap();
            let mut result = result;
            result["operation_id"] = json!(id);
            result["requested_muted"] = json!(true);
            a.complete(id, result, Instant::now());
            assert_eq!(a.view("browser-a", id).1["state"], "outcome_unconfirmed");
        }
    }
    #[test]
    fn lost_response_can_poll_same_receipt_without_replay() {
        let mut a = Actions::default();
        let p = prepare(&mut a);
        let id = p["operation_id"].as_str().unwrap();
        a.consume("browser-a", &body(&p)).unwrap();
        a.complete(id,json!({"operation_id":id,"requested_muted":true,"state":"verified","observed_muted":true,"error":null}),Instant::now());
        let receipt = a.view("browser-a", id).1;
        assert_eq!(receipt["state"], "verified");
        assert_eq!(receipt["observed_muted"], true);
        assert!(receipt["readback_age_ms"].is_number());
        assert_eq!(a.consume("browser-a", &body(&p)).unwrap_err().0, 409);
        assert_eq!(Actions::default().view("browser-a", id).0, 404);
    }
    #[test]
    fn receipts_are_bounded_and_rejection_is_terminal() {
        let mut a = Actions::default();
        for _ in 0..LIMIT {
            let p = prepare(&mut a);
            let id = p["operation_id"].as_str().unwrap();
            a.consume("browser-a", &body(&p)).unwrap();
            a.reject(id, "queue_full");
        }
        assert_eq!(a.operations.len(), LIMIT);
        assert_eq!(a.prepare("a", "e", "k", json!({}), "Fixture", true).0, 409);
    }
}
