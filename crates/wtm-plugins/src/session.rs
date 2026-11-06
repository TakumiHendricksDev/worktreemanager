//! One bounded RPC per invocation lets a transport reconnect without restarting its session owner.
//! The helper may talk to a persistent daemon over SSH; EOF on this pipe is never a stop request.
use crate::Store;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    List,
    Create,
    Attach,
    Poll,
    Control,
    Input,
    Resize,
    Interrupt,
    Detach,
    Stop,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub method: Method,
    #[serde(default)]
    pub params: Value,
}
impl Store {
    pub fn session_call(
        &self,
        plugin: &str,
        backend: &str,
        request: &Request,
        cwd: &Path,
    ) -> Result<Value, String> {
        let _running = self.activation(plugin);
        let id = uuid::Uuid::new_v4().to_string();
        let wire = json!({"jsonrpc":"2.0", "id":id, "method":request.method, "params":request.params, "api":1}).to_string();
        if wire.len() > 65536 {
            return Err("Session request exceeds 64 KiB".into());
        }
        if matches!(request.method, Method::Detach)
            && let Some(client) = request.params.get("client").and_then(Value::as_str)
            && let Some(session) = request.params.get("session").and_then(Value::as_str)
        {
            let key = format!("{backend}:{session}");
            self.inner
                .lock()
                .attached
                .retain(|(p, s, c)| p != plugin || s != &key || c != client);
        }
        let out = self.invoke(plugin, backend, true, &wire, cwd)?;
        let result = decode(&out, &id)?;
        if let (Some(session), Some(client)) = (
            request.params.get("session").and_then(Value::as_str),
            request.params.get("client").and_then(Value::as_str),
        ) {
            let key = (
                plugin.to_owned(),
                format!("{backend}:{session}"),
                client.to_owned(),
            );
            match request.method {
                Method::Attach => {
                    self.inner.lock().attached.insert(key);
                }
                Method::Detach => {
                    self.inner.lock().attached.remove(&key);
                }
                Method::Stop => {
                    self.inner
                        .lock()
                        .attached
                        .retain(|(p, s, _)| p != plugin || s != &key.1);
                }
                _ => {}
            }
        }
        Ok(result)
    }
}
pub fn decode(out: &str, id: &str) -> Result<Value, String> {
    let value: Value =
        serde_json::from_str(out).map_err(|e| format!("Invalid backend response: {e}"))?;
    if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || value.get("id").and_then(Value::as_str) != Some(id)
    {
        return Err("Backend returned a mismatched RPC response".into());
    }
    if let Some(error) = value.get("error") {
        return Err(format!(
            "Session backend: {}",
            error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("request failed")
        ));
    }
    value
        .get("result")
        .cloned()
        .ok_or_else(|| "Backend response has no result".into())
}
