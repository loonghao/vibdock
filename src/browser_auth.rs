//! Instance-local, one-use browser handoff codes and scoped sessions.
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use crate::{service, Result};

#[derive(Default)]
pub(crate) struct BrowserAuth {
    codes: HashMap<String, Instant>,
    sessions: HashMap<String, Instant>,
}

impl BrowserAuth {
    fn prune(&mut self) {
        let now = Instant::now();
        self.codes.retain(|_, expires| *expires > now);
        self.sessions.retain(|_, expires| *expires > now);
    }

    pub(crate) fn mint(&mut self) -> Result<String> {
        self.prune();
        if self.codes.len() >= 32 {
            return Err("Too many pending browser authorizations".into());
        }
        let code = service::generate_token()?;
        self.codes
            .insert(code.clone(), Instant::now() + Duration::from_secs(60));
        Ok(code)
    }

    pub(crate) fn redeem(&mut self, code: &str) -> Result<Option<String>> {
        self.prune();
        if !self.codes.contains_key(code) {
            return Ok(None);
        }
        if self.sessions.len() >= 32 {
            return Err("Too many active browser sessions".into());
        }
        let token = service::generate_token()?;
        self.codes.remove(code);
        self.sessions.insert(
            token.clone(),
            Instant::now() + Duration::from_secs(8 * 3600),
        );
        Ok(Some(token))
    }

    pub(crate) fn accepts(&mut self, token: Option<&str>) -> bool {
        self.prune();
        token.is_some_and(|token| self.sessions.contains_key(token))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_one_use_and_sessions_are_instance_local() {
        let mut auth = BrowserAuth::default();
        let code = auth.mint().unwrap();
        assert!(!auth.accepts(Some(&code)));
        assert!(auth.redeem("wrong").unwrap().is_none());
        let token = auth.redeem(&code).unwrap().unwrap();
        assert!(auth.accepts(Some(&token)));
        assert!(auth.redeem(&code).unwrap().is_none());
        assert!(!BrowserAuth::default().accepts(Some(&token)));
    }

    #[test]
    fn expired_codes_and_sessions_are_rejected() {
        let mut auth = BrowserAuth::default();
        auth.codes.insert("expired".into(), Instant::now());
        auth.sessions.insert("expired".into(), Instant::now());
        assert!(auth.redeem("expired").unwrap().is_none());
        assert!(!auth.accepts(Some("expired")));
    }

    #[test]
    fn outstanding_codes_are_bounded() {
        let mut auth = BrowserAuth::default();
        for _ in 0..32 {
            auth.mint().unwrap();
        }
        assert!(auth.mint().is_err());
    }
}
