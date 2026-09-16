//! Public approval identities survive reconnect and cannot alias another vendor process.
use anyhow::Result;
use base64::Engine;
use std::{collections::BTreeMap, sync::Mutex};
use sylph_protocol::{AdapterError, Request, Resolution};

pub(super) struct RequestScope {
    nonce: String,
    native: Mutex<BTreeMap<String, String>>,
}
impl Default for RequestScope {
    fn default() -> Self {
        Self {
            nonce: uuid::Uuid::new_v4().to_string(),
            native: Mutex::new(BTreeMap::new()),
        }
    }
}
impl RequestScope {
    pub fn publish(&self, mut request: Request) -> Request {
        let native = request.id.clone();
        request.id = format!(
            "rq:{}:{}",
            self.nonce,
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(native.as_bytes())
        );
        self.native
            .lock()
            .unwrap()
            .insert(request.id.clone(), native);
        request
    }
    pub fn resolve(&self, mut resolution: Resolution) -> Result<Resolution> {
        resolution.request_id = self
            .native
            .lock()
            .unwrap()
            .get(&resolution.request_id)
            .cloned()
            .ok_or_else(|| {
                AdapterError::invalid_value("request is not pending on this vendor process")
            })?;
        Ok(resolution)
    }
    pub fn retire(&self, public: &str) {
        self.native.lock().unwrap().remove(public);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn request_identity_replays_within_one_process_and_cannot_resolve_on_its_successor() {
        let old = RequestScope::default();
        let new = RequestScope::default();
        let native: Request =
            serde_json::from_value(serde_json::json!({"id":"1:/vendor/id","kind":"approval"}))
                .unwrap();
        let public = old.publish(native.clone());
        assert_eq!(old.publish(native.clone()).id, public.id);
        let successor = new.publish(native.clone());
        assert_ne!(successor.id, public.id);
        let resolution: Resolution =
            serde_json::from_value(serde_json::json!({"requestId":public.id,"option":"accept"}))
                .unwrap();
        assert_eq!(
            old.resolve(resolution.clone()).unwrap().request_id,
            native.id
        );
        assert!(new.resolve(resolution.clone()).is_err());
        old.retire(&public.id);
        assert!(old.resolve(resolution).is_err());
    }
}
