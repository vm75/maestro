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
    match row.get("Labels") {
        Some(Value::Object(map)) => map.iter().map(|(k,v)| {
            Ok((k.clone(), v.as_str().ok_or_else(|| anyhow!("invalid native label"))?.to_owned()))
        }).collect(),
        Some(Value::String(s)) => Ok(s.split(',').filter_map(|part| part.split_once('='))
            .map(|(k,v)| (k.trim().to_owned(), v.to_owned())).collect()),
        // A missing label field cannot establish that a custom-named container is external.
        Some(Value::Null) => Ok(BTreeMap::new()),
        _ => Err(anyhow!("native container labels unavailable")),
    }
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
        let name = row.get("Names").and_then(Value::as_array)
            .and_then(|names| names.first()).and_then(Value::as_str)
            .unwrap_or_else(|| string(&row, &["Names", "Name"])).trim_start_matches('/');
        let labels = labels(&row)?;
        if !valid_text(name) || !valid_text(id) || reserved(name)
            || labels.keys().any(|key| key.starts_with("io.maestro."))
            || labels.iter().any(|(key,value)| key.ends_with(".compose.project") && reserved(value))
            || config.services.values().any(|service| service.engine == engine &&
                service.containers.values().any(|container| container.container_name == name)) {
            continue;
        }
        let state = string(&row, &["State", "Status"]);
        let image = string(&row, &["Image"]);
        result.push(NativeWorkload {
            provider: engine.into(), scope: "local".into(), id: id.into(),
            name: name.into(), kind: "application-container".into(),
            state: if valid_text(state) { state.to_owned() } else { "unknown".into() },
            image: if valid_text(image) { image.to_owned() } else { String::new() },
            addresses: vec![],
        });
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
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
