//! Explicit, bounded microphone diagnostics. Audio samples never enter this API.

use crate::web::{Method, Request, Routed};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

struct Job {
    owner: String,
    device_id: String,
    stop: Arc<AtomicBool>,
    receiver: mpsc::Receiver<(Value, bool)>,
    handle: Option<JoinHandle<()>>,
    receipt: Value,
    finished: Option<Instant>,
}

#[derive(Default)]
pub(crate) struct AudioTests {
    jobs: BTreeMap<String, Job>,
    cleanup_unconfirmed: bool,
}

impl AudioTests {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn poll(&mut self) {
        for job in self.jobs.values_mut().filter(|job| job.finished.is_none()) {
            match job.receiver.try_recv() {
                Ok((result, cleanup)) => {
                    if !cleanup {
                        self.cleanup_unconfirmed = true;
                    }
                    let matching = result["device_id"].as_str() == Some(job.device_id.as_str());
                    job.receipt["state"] =
                        json!(if cleanup && matching && result["error"].is_null() {
                            "completed"
                        } else {
                            "failed"
                        });
                    job.receipt["result"] = if cleanup && matching {
                        result
                    } else {
                        Value::Null
                    };
                    job.receipt["error"] = if !cleanup {
                        json!("Native test cleanup was not confirmed")
                    } else if !matching {
                        json!("Native test returned a different microphone")
                    } else {
                        job.receipt["result"]["error"].clone()
                    };
                    job.finished = Some(Instant::now());
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.cleanup_unconfirmed = true;
                    job.receipt["state"] = json!("failed");
                    job.receipt["error"] = json!("Native test worker ended without a result");
                    job.finished = Some(Instant::now());
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
            if job.finished.is_some() {
                if let Some(handle) = job.handle.take() {
                    let _ = handle.join();
                }
            }
        }
        self.jobs.retain(|_, job| {
            job.finished
                .is_none_or(|at| at.elapsed() < Duration::from_secs(60))
        });
    }

    pub(crate) fn cancel(&mut self) {
        for job in self.jobs.values() {
            job.stop.store(true, Ordering::Release);
        }
    }

    #[cfg(any(windows, test))]
    fn start_with(
        &mut self,
        owner: &str,
        device_id: String,
        run: impl FnOnce(Arc<AtomicBool>) -> (Value, bool) + Send + 'static,
    ) -> Result<Value, &'static str> {
        self.poll();
        if self.cleanup_unconfirmed {
            return Err(
                "Native test cleanup is unconfirmed; restart the service before another test",
            );
        }
        if self.jobs.values().any(|job| job.finished.is_none()) {
            return Err("A microphone test is already running");
        }
        if self.jobs.len() >= 4 {
            return Err("Microphone test capacity reached; try again later");
        }
        let id = crate::service::generate_token().map_err(|_| "Could not create test identity")?;
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let (sender, receiver) = mpsc::sync_channel(1);
        let handle = std::thread::Builder::new()
            .name("vibdock-microphone-test".into())
            .spawn(move || {
                let result = run(worker_stop);
                let _ = sender.send(result);
            })
            .map_err(|_| "Could not start microphone test worker")?;
        let receipt = json!({"operation_id":id,"device_id":device_id,"state":"pending","result":null,"error":null});
        self.jobs.insert(
            id,
            Job {
                owner: owner.to_owned(),
                device_id,
                stop,
                receiver,
                handle: Some(handle),
                receipt: receipt.clone(),
                finished: None,
            },
        );
        Ok(receipt)
    }

    fn read(&mut self, owner: &str, id: &str) -> Option<Value> {
        self.poll();
        self.jobs
            .get(id)
            .filter(|job| job.owner == owner)
            .map(|job| job.receipt.clone())
    }
}

impl Drop for AudioTests {
    fn drop(&mut self) {
        self.cancel();
        // The owned helper checks cancellation and terminates its process job.
        for job in self.jobs.values_mut() {
            if let Some(handle) = job.handle.take() {
                let _ = handle.join();
            }
        }
    }
}

fn identifier(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn device(body: &str) -> Result<String, &'static str> {
    let value: Value =
        crate::bindings::strict_json(body).map_err(|_| "Expected an unambiguous JSON object")?;
    let object = value
        .as_object()
        .filter(|object| object.len() == 1 && object.contains_key("device_id"))
        .ok_or("Only device_id is accepted")?;
    object["device_id"]
        .as_str()
        .filter(|id| !id.trim().is_empty() && id.len() <= 512 && !id.chars().any(char::is_control))
        .map(str::to_owned)
        .ok_or("An exact microphone endpoint ID is required")
}

pub(crate) fn route(
    request: &Request<'_>,
    token_configured: bool,
    tests: &mut AudioTests,
    principal: Option<&str>,
) -> Option<Routed> {
    if request.path != "/api/audio/tests" && !request.path.starts_with("/api/audio/tests/") {
        return None;
    }
    if let Some(rejection) = crate::web::validate_request(request, token_configured) {
        return Some(rejection);
    }
    if !request.authorized || principal.is_none() {
        return Some(crate::web::unauthorized());
    }
    let owner = principal.unwrap();
    let reply = |code, value: Value| Some((code, "application/json", value.to_string()));
    if request.method == Method::Post && request.path == "/api/audio/tests" {
        let device_id = match device(request.body) {
            Ok(id) => id,
            Err(error) => return reply(400, json!({"error":error})),
        };
        #[cfg(windows)]
        {
            let executable = match std::env::current_exe() {
                Ok(path) => path,
                Err(_) => return reply(503, json!({"error":"Native helper is unavailable"})),
            };
            let target = device_id.clone();
            return match tests.start_with(owner, device_id, move |stop| {
                crate::resident_apps::run_microphone_test(&executable, &stop, &target)
            }) {
                Ok(receipt) => reply(202, receipt),
                Err(error) => reply(409, json!({"error":error})),
            };
        }
        #[cfg(not(windows))]
        {
            let _ = device_id;
            return reply(
                501,
                json!({"error":"Microphone tests require the Windows audio adapter"}),
            );
        }
    }
    let tail = request.path.strip_prefix("/api/audio/tests/").unwrap_or("");
    let (id, cancelling) = tail
        .strip_suffix("/cancel")
        .map_or((tail, false), |id| (id, true));
    if !identifier(id) {
        return reply(404, json!({"error":"Unknown microphone test"}));
    }
    if request.method == Method::Get && !cancelling {
        return match tests.read(owner, id) {
            Some(receipt) => reply(200, receipt),
            None => reply(404, json!({"error":"Unknown microphone test"})),
        };
    }
    if request.method == Method::Post && cancelling {
        if !matches!(request.body.trim(), "" | "{}") {
            return reply(400, json!({"error":"Cancel takes no fields"}));
        }
        tests.poll();
        let Some(job) = tests.jobs.get_mut(id).filter(|job| job.owner == owner) else {
            return reply(404, json!({"error":"Unknown microphone test"}));
        };
        job.stop.store(true, Ordering::Release);
        return reply(
            202,
            json!({"operation_id":id,"state":"cancellation_requested"}),
        );
    }
    reply(405, json!({"error":"Unsupported microphone test method"}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_is_closed_and_requires_exact_identity() {
        for bad in [
            "{}",
            "{\"device_id\":null}",
            "{\"device_id\":\"\"}",
            "{\"device_id\":\"mic\",\"unmute\":true}",
            "{\"device_id\":\"a\\n\"}",
        ] {
            assert!(device(bad).is_err(), "{bad}");
        }
        assert_eq!(
            device("{\"device_id\":\"exact-microphone\"}").unwrap(),
            "exact-microphone"
        );
    }

    #[test]
    fn result_requires_matching_microphone_and_confirmed_cleanup() {
        for (returned, cleanup) in [("different", true), ("chosen", false)] {
            let mut tests = AudioTests::new();
            let receipt = tests
                .start_with("owner", "chosen".into(), move |_| {
                    (json!({"device_id":returned,"error":null}), cleanup)
                })
                .unwrap();
            let id = receipt["operation_id"].as_str().unwrap();
            while !tests.jobs[id].handle.as_ref().unwrap().is_finished() {
                std::thread::yield_now();
            }
            let result = tests.read("owner", id).unwrap();
            assert_eq!(result["state"], "failed");
            assert!(result["result"].is_null());
            assert!(tests.read("other", id).is_none());
            if !cleanup {
                assert!(tests
                    .start_with("owner", "chosen".into(), |_| (Value::Null, true))
                    .is_err());
            }
        }
    }

    #[test]
    fn one_pending_test_and_explicit_cancellation() {
        let mut tests = AudioTests::new();
        let receipt = tests
            .start_with("owner", "chosen".into(), |stop| {
                while !stop.load(Ordering::Acquire) {
                    std::thread::yield_now();
                }
                (json!({"device_id":"chosen","error":"cancelled"}), true)
            })
            .unwrap();
        assert!(tests
            .start_with("owner", "other".into(), |_| (Value::Null, true))
            .is_err());
        tests.cancel();
        let id = receipt["operation_id"].as_str().unwrap();
        while !tests.jobs[id].handle.as_ref().unwrap().is_finished() {
            std::thread::yield_now();
        }
        let result = tests.read("owner", id).unwrap();
        assert_eq!(result["state"], "failed");
        assert_eq!(result["result"]["error"], "cancelled");
    }
}
