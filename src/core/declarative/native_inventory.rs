//! Native inventory of externally provisioned workloads. Ported from the
//! original Maestro runtime adapters, without their name-only mutation path.
use anyhow::{Result, ensure, anyhow};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use super::Config;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeWorkload {
    pub provider: String,
    pub scope: String,
    pub id: String,
    pub name: String,
    pub kind: String,
    pub state: String,
    pub image: String,
    pub addresses: Vec<String>,
    pub actions: Vec<&'static str>,
    pub logs: bool,
    pub identity: String,
}

#[derive(Debug, Default, Serialize)]
pub struct NativeInventory {
    pub workloads: Vec<NativeWorkload>,
    pub warnings: Vec<String>,
}

pub(super) fn reserved(name: &str) -> bool {
    name.starts_with("maestro-")
}

pub(super) fn valid_text(s: &str) -> bool {
    !s.is_empty() && s.len() <= 512 && !s.chars().any(char::is_control)
}

fn string<'a>(row: &'a Value, keys: &[&str]) -> &'a str {
    keys.iter().find_map(|key| row.get(*key).and_then(Value::as_str)).unwrap_or("")
}

fn labels(row: &Value) -> Result<BTreeMap<String, String>> {
    match row.get("Labels").or_else(|| row.get("labels")) {
        Some(Value::Object(map)) => map.iter().map(|(k,v)| {
            Ok((k.clone(), v.as_str().ok_or_else(|| anyhow!("invalid native label"))?.to_owned()))
        }).collect(),
        Some(Value::String(s)) => {
            let mut labels = BTreeMap::new();
            for part in s.split(',').filter(|s| !s.is_empty()) {
                let (key,value) = part.split_once('=').ok_or_else(|| anyhow!("ambiguous native labels"))?;
                ensure!(labels.insert(key.trim().to_owned(),value.to_owned()).is_none(), "duplicate native label");
            }
            Ok(labels)
        },
        // A missing label field cannot establish that a custom-named container is external.
        Some(Value::Null) => Ok(BTreeMap::new()),
        _ => Err(anyhow!("native container labels unavailable")),
    }
}

pub(super) fn full_oci_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn orchestrated(labels: &BTreeMap<String, String>) -> bool {
    labels.keys().any(|key| {
        let key = key.to_ascii_lowercase();
        ["com.docker.compose.", "io.podman.compose.", "com.docker.swarm.", "io.kubernetes.",
         "io.openshift.", "io.podman.", "io.containers.", "podman_systemd_unit", "org.systemd."].iter().any(|prefix| key.starts_with(*prefix))
    })
}

fn owned(config: &Config, engine: &str, name: &str, labels: &BTreeMap<String,String>) -> bool {
    reserved(name) || labels.keys().any(|key| key.starts_with("io.maestro."))
        || labels.iter().any(|(key,value)| key.ends_with(".compose.project") && reserved(value))
        || config.services.values().any(|service| service.engine == engine &&
            service.containers.values().any(|container| container.container_name == name))
}

/// Check actual inspect data, not the formatted ps labels, before any native action.
pub(super) fn verify_oci(data: &[u8], engine: &str, config: &Config, id: &str, for_action: bool) -> Result<()> {
    let rows: Vec<Value> = serde_json::from_slice(data)?;
    ensure!(rows.len() == 1 && full_oci_id(id), "ambiguous container identity");
    let row = &rows[0];
    ensure!(string(row, &["Id","ID","id"]) == id, "container identity changed");
    let name = string(row, &["Name","name"]).trim_start_matches('/');
    ensure!(valid_text(name), "native container name unavailable");
    let raw = row.pointer("/Config/Labels").ok_or_else(|| anyhow!("native container labels unavailable"))?;
    let labels = labels(&serde_json::json!({"Labels":raw}))?;
    ensure!(!owned(config,engine,name,&labels), "managed or reserved workload");
    if for_action {
        ensure!(!orchestrated(&labels), "externally orchestrated container; use its original orchestrator");
        ensure!(row.get("Pod").and_then(Value::as_str).is_none_or(str::is_empty), "Podman pod member; use its original orchestrator");
    }
    Ok(())
}

pub(super) fn oci_rows(data: &[u8], engine: &str, config: &Config) -> Result<Vec<NativeWorkload>> {
    let rows: Vec<Value> = match serde_json::from_slice::<Value>(data) {
        Ok(Value::Array(rows)) => rows,
        Ok(Value::Object(row)) => vec![Value::Object(row)],
        _ => std::str::from_utf8(data)?.lines().filter(|line| !line.trim().is_empty())
            .map(serde_json::from_str).collect::<Result<_, _>>()?,
    };
    ensure!(rows.len() <= 1024, "native container inventory exceeds limit");
    let mut result = Vec::new();
    for row in rows {
        let id = string(&row, &["ID", "Id", "id"]);
        let name = row.get("Names").or_else(|| row.get("names")).and_then(Value::as_array)
            .and_then(|names| names.first()).and_then(Value::as_str)
            .unwrap_or_else(|| string(&row, &["Names", "Name", "names", "name"])).trim_start_matches('/');
        let labels = labels(&row)?;
        if !valid_text(name) || !valid_text(id) || owned(config,engine,name,&labels) {
            continue;
        }
        let state = string(&row, &["State", "state", "Status", "status"]);
        let image = string(&row, &["Image", "image"]);
        let actions = if full_oci_id(id) && !orchestrated(&labels) {
            match state.to_ascii_lowercase().as_str() {
                "running" => vec!["stop","restart"],
                "created" | "configured" | "exited" | "stopped" => vec!["start","delete"],
                _ => vec![],
            }
        } else { vec![] };
        result.push(NativeWorkload {
            provider: engine.into(), scope: "local".into(), id: id.into(),
            name: name.into(), kind: "application-container".into(),
            state: if valid_text(state) { state.to_owned() } else { "unknown".into() },
            image: if valid_text(image) { image.to_owned() } else { String::new() },
            addresses: vec![], actions, logs: full_oci_id(id), identity: id.into(),
        });
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn full_identity_and_orchestration_checks() {
        let id = "a".repeat(64);
        let config = Config::default();
        let row = serde_json::json!([{"Id":id,"Name":"/external","Config":{"Labels":null}}]);
        assert!(verify_oci(row.to_string().as_bytes(),"docker",&config,&id,true).is_ok());
        let row = serde_json::json!([{"Id":id,"Name":"/external","Config":{"Labels":{"com.docker.compose.project":"other"}}}]);
        assert!(verify_oci(row.to_string().as_bytes(),"docker",&config,&id,true).is_err());
        assert!(verify_oci(row.to_string().as_bytes(),"docker",&config,&id,false).is_ok());
        let row = serde_json::json!([{"Id":id,"Name":"/external","Config":{"Labels":{"io.maestro.service":"old"}}}]);
        assert!(verify_oci(row.to_string().as_bytes(),"docker",&config,&id,false).is_err());
        assert!(!full_oci_id("1234"));
    }
    #[test]
    fn docker_json_lines_and_podman_array_exclude_reserved_and_owned() {
        let config = super::super::parse(b"version: 1\nservices:\n  web: {kind: container, engine: docker, containers: {app: {image: nginx, container_name: custom}}}\n").unwrap();
        let input = br#"{"ID":"abc","Names":"outside","State":"running","Image":"nginx","Labels":""}
{"ID":"def","Names":"custom","Labels":""}
{"ID":"ghi","Names":"renamed","Labels":"io.maestro.service=web"}
{"ID":"jkl","Names":"maestro-stale","Labels":""}
"#;
        let rows = oci_rows(input,"docker",&config).unwrap();
        assert_eq!(rows.len(),1);
        assert_eq!(rows[0].name,"outside");
        let input = br#"[{"Id":"123","Names":["test"],"State":"exited","Labels":{}},{"Id":"456","Names":["renamed"],"Labels":{"io.podman.compose.project":"maestro-app"}}]"#;
        assert_eq!(oci_rows(input,"podman",&config).unwrap().len(),1);
        assert!(oci_rows(br#"[{"ID":"abc","Names":"unknown"}]"#,"docker",&config).is_err());
    }
}
