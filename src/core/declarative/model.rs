use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub version: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server: Option<serde_yaml::Value>,
    pub services: BTreeMap<String, Service>,
    pub storage: BTreeMap<String, Storage>,
    pub shares: BTreeMap<String, Share>,
    pub bindings: BTreeMap<String, Binding>,
    pub secrets: BTreeMap<String, String>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct DiskSource {
    pub path: String,
    pub format: String,
    pub sha256: String,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Storage {
    pub source: Option<DiskSource>,
    pub format: String,
    pub path: String,
    pub external: bool,
    pub kind: String,
    pub mount: String,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Binding {
    pub kind: String,
    pub name: String,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Service {
    pub name: String,
    pub description: String,
    pub kind: String,
    pub engine: String,
    pub backend: String,
    pub enabled: bool,
    pub depends_on: Vec<String>,
    pub containers: BTreeMap<String, Container>,
    pub vm: Option<VM>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Container {
    pub image: String,
    pub container_name: String,
    pub command: Vec<String>,
    pub entrypoint: Vec<String>,
    pub working_dir: String,
    pub ports: Vec<String>,
    pub environment: BTreeMap<String, String>,
    pub env_files: Vec<String>,
    pub mounts: Vec<Mount>,
    pub memory_mib: i64,
    pub cpus: i64,
    pub user: String,
    pub userns_mode: String,
    pub uid_maps: Vec<String>,
    pub gid_maps: Vec<String>,
    pub group_add: Vec<String>,
    pub network_mode: String,
    pub depends_on: BTreeMap<String, String>,
    pub devices: Vec<String>,
    pub cap_add: Vec<String>,
    pub cap_drop: Vec<String>,
    pub security_opt: Vec<String>,
    pub privileged: bool,
    pub read_only: bool,
    pub init: bool,
    pub tty: bool,
    pub tmpfs: Vec<String>,
    pub shm_size: String,
    pub stop_grace_period: String,
    pub pull_policy: String,
    pub restart: String,
    pub log_driver: String,
    pub log_options: BTreeMap<String, String>,
    pub healthcheck: Option<ContainerHealth>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ContainerHealth {
    pub test: Vec<String>,
    pub interval: String,
    pub timeout: String,
    pub retries: i64,
    pub start_period: String,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Mount {
    pub storage: String,
    pub path: String,
    pub kind: String,
    pub target: String,
    pub read_only: bool,
    pub selinux: String,
    pub chown: bool,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct VM {
    pub root_size_gib: i64,
    pub architecture: String,
    pub machine: String,
    pub firmware: String,
    pub uuid: String,
    pub mac: String,
    pub image: String,
    pub cpus: i64,
    pub memory_mib: i64,
    pub disks: Vec<String>,
    pub network: String,
    pub pool: String,
    pub project: String,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Share {
    pub guest: bool,
    pub browseable: bool,
    pub root_squash: Option<bool>,
    pub security: String,
    pub protocol: String,
    pub storage: String,
    pub path: String,
    pub enabled: bool,
    pub read_only: bool,
    pub users: Vec<String>,
    pub clients: Vec<String>,
}
