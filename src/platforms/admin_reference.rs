//! Direct port of Server Man internal/admin/helper.go and core/schema.go.
//! The public helper envelope is independent of the Maestro HTTP/job transport.
use std::{
    collections::BTreeMap,
    os::fd::OwnedFd,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::{io::{AsyncRead, AsyncReadExt}, process::Command, time::timeout};

use super::{Policy, admin_packages, admin_shares, safe_system_unit};

pub(super) const MAX_REQUEST: usize = 1 << 20;
const MAX_CAPTURE: usize = 1 << 20;
const MAX_TAIL: usize = 128 << 10;

const OPERATIONS: &[&str] = &[
    "shares_definitions_validate", "shares_definitions_inspect", "shares_definitions_apply",
    "dependencies_install", "shares_setup", "packages_install", "updates_status",
    "updates_refresh", "updates_install", "shares_list", "shares_samba_password",
    "services_list", "services_details", "services_logs", "services_start",
    "services_stop", "services_restart", "services_reload", "services_enable", "services_disable",
];

#[derive(Serialize, Deserialize)]
pub(super) struct Request {
    pub operation: String,
    pub arguments: Value,
}

#[derive(Default, Debug, Serialize, Deserialize)]
pub(super) struct Response {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub output: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub code: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub status: u16,
}

fn is_false(value: &bool) -> bool { !*value }
fn is_zero(value: &u16) -> bool { *value == 0 }

#[derive(Debug)]
pub(super) struct Failure {
    message: String,
    code: &'static str,
    status: u16,
}
type OperationResult<T> = std::result::Result<T, Failure>;

impl Failure {
    fn new(code: &'static str, status: u16, message: impl Into<String>) -> Self {
        Self { message: message.into(), code, status }
    }
    fn invalid(message: impl Into<String>) -> Self { Self::new("invalid_arguments", 400, message) }
    fn unavailable(message: impl Into<String>) -> Self { Self::new("unavailable", 503, message) }
    fn failed(message: impl Into<String>) -> Self { Self::new("operation_failed", 500, message) }
    fn denied(message: impl Into<String>) -> Self { Self::new("permission_denied", 403, message) }
}

pub(super) fn decode(data: &[u8]) -> Result<Request> {
    ensure!(data.len() <= MAX_REQUEST, "request exceeds 1 MiB");
    let mut decoder = serde_json::Deserializer::from_slice(data);
    let value = Value::deserialize(&mut decoder).map_err(|_| anyhow::anyhow!("invalid helper request"))?;
    let mut operation = String::new();
    let mut arguments = json!({});
    if !value.is_null() {
        let fields = value.as_object().context("invalid helper request")?;
        for (key, value) in fields {
            match key.to_ascii_lowercase().as_str() {
                "operation" if value.is_null() => {}
                "operation" => operation = value.as_str().context("invalid helper request")?.to_owned(),
                "arguments" => arguments = value.clone(),
                _ => anyhow::bail!("invalid helper request"),
            }
        }
    }
    ensure!(decoder.end().is_ok(), "expected one helper request");
    ensure!(OPERATIONS.contains(&operation.as_str()), "operation is not allowed");
    if operation.starts_with("services_") {
        if let Some(scope) = arguments.get("scope").and_then(Value::as_str) {
            ensure!(scope.is_empty() || scope == "system", "helper manages system services only");
        }
    }
    Ok(Request { operation, arguments })
}

// The frozen registry was extracted from the pinned Go implementation. The
// three helper-only schemas are declared in shares/definitions.go.
fn schema(operation: &str) -> Value {
    if operation.starts_with("shares_definitions_") {
        let mut properties = json!({
            "protocol":{"type":"string","enum":["samba","nfs"]},
            "definitions":{"type":"string","minLength":1,"maxLength":262144}
        });
        let mut required = vec!["protocol", "definitions"];
        if operation == "shares_definitions_apply" {
            properties["revision"] = json!({"type":"string","minLength":1,"maxLength":64});
            properties["confirm"] = json!({"type":"boolean"});
            required.extend(["revision","confirm"]);
        }
        return json!({"properties":properties,"required":required});
    }
    let registry: Value = serde_json::from_str(include_str!(
        "../../test-fixtures/phase2/server-man-tools.json"
    )).expect("checked-in Server Man registry fixture");
    registry["tools"].as_array().expect("tool array")
        .iter().find(|tool| tool["name"] == operation)
        .expect("allowlisted reference tool has a frozen schema")["inputSchema"].clone()
}

fn quoted(key: &str) -> String { serde_json::to_string(key).expect("string encoding") }

fn validate(request: &Request) -> OperationResult<&Map<String, Value>> {
    let input = request.arguments.as_object().ok_or_else(|| Failure::invalid("arguments must be an object"))?;
    let schema = schema(&request.operation);
    let fields = schema["properties"].as_object().expect("schema properties");
    for (key, value) in input {
        let field = fields.get(key).ok_or_else(|| Failure::invalid(format!("unknown argument {}", quoted(key))))?;
        match field["type"].as_str().unwrap_or_default() {
            "string" => {
                let text = value.as_str().ok_or_else(|| Failure::invalid(format!("{key} must be a string")))?;
                let length = text.chars().count() as u64;
                if field["minLength"].as_u64().is_some_and(|min| length < min) {
                    return Err(Failure::invalid(format!("{key} is too short")));
                }
                if field["maxLength"].as_u64().is_some_and(|max| length > max) {
                    return Err(Failure::invalid(format!("{key} is too long")));
                }
                if field["enum"].as_array().is_some_and(|values| !values.contains(value)) {
                    return Err(Failure::invalid(format!("unsupported {key}")));
                }
            }
            "boolean" if !value.is_boolean() => return Err(Failure::invalid(format!("{key} must be a boolean"))),
            "boolean" => {}
            kind @ ("integer" | "number") => {
                let n = value.as_f64().filter(|n| n.is_finite() && (kind != "integer" || n.fract() == 0.0))
                    .ok_or_else(|| Failure::invalid(format!("{key} must be {kind}")))?;
                if field["minimum"].as_f64().is_some_and(|min| n < min) {
                    return Err(Failure::invalid(format!("{key} is below its minimum")));
                }
                if field["maximum"].as_f64().is_some_and(|max| n > max) {
                    return Err(Failure::invalid(format!("{key} exceeds its maximum")));
                }
            }
            _ => return Err(Failure::failed("unsupported registered field type")),
        }
    }
    for key in schema["required"].as_array().expect("schema required") {
        let key = key.as_str().expect("required field name");
        if !input.contains_key(key) {
            return Err(Failure::invalid(format!("missing argument {}", quoted(key))));
        }
    }
    if fields.contains_key("confirm") && input.get("confirm") != Some(&Value::Bool(true)) {
        return Err(Failure::invalid("confirm must be true for this operation"));
    }
    Ok(input)
}

fn string<'a>(input: &'a Map<String, Value>, key: &str) -> &'a str {
    input.get(key).and_then(Value::as_str).unwrap_or_default()
}

#[derive(Default)]
pub(super) struct Tail {
    data: Vec<u8>,
    truncated: bool,
}
impl Tail {
    fn append(&mut self, bytes: &[u8], limit: usize) {
        if bytes.len() >= limit {
            self.truncated |= !self.data.is_empty() || bytes.len() > limit;
            self.data.clear();
            self.data.extend_from_slice(&bytes[bytes.len()-limit..]);
        } else {
            let excess = (self.data.len()+bytes.len()).saturating_sub(limit);
            self.truncated |= excess > 0;
            self.data.drain(..excess);
            self.data.extend_from_slice(bytes);
        }
    }
    fn merge(&mut self, tail: Tail, limit: usize) {
        self.truncated |= tail.truncated;
        self.append(&tail.data, limit);
    }
    fn text(&self) -> String { String::from_utf8_lossy(&self.data).into_owned() }
}

async fn drain(mut reader: impl AsyncRead + Unpin, limit: usize) -> std::io::Result<Tail> {
    let mut result = Tail::default();
    let mut bytes = [0u8; 8192];
    loop {
        let n = reader.read(&mut bytes).await?;
        if n == 0 { return Ok(result); }
        result.append(&bytes[..n], limit);
    }
}

pub(super) fn program(name: &str) -> Option<PathBuf> {
    // Fixed search path; preserve the stronger Maestro executable checks.
    ["/usr/sbin", "/usr/bin", "/sbin", "/bin"].iter().find_map(|dir| {
        admin_packages::trusted_executable(&Path::new(dir).join(name)).ok()
    })
}

struct Group(Option<u32>);
impl Drop for Group {
    fn drop(&mut self) {
        if let Some(pid) = self.0 { unsafe { libc::kill(-(pid as i32), libc::SIGKILL); } }
    }
}

fn command(program: &Path, args: &[&str], environment: &[(&str,&str)]) -> Command {
    let mut command = Command::new(program);
    command.args(args).env_clear().envs([
        ("PATH","/usr/sbin:/usr/bin:/sbin:/bin"), ("HOME","/root"),
        ("LANG","C"), ("LC_ALL","C"), ("SYSTEMD_PAGER","cat"),
        ("PAGER","cat"), ("TERM","dumb"),
    ]).envs(environment.iter().copied()).current_dir("/")
      .stdin(Stdio::null()).kill_on_drop(true).process_group(0);
    command
}

pub(super) struct Captured {
    pub text: String,
    pub stderr: String,
    pub code: Option<i32>,
}

pub(super) async fn capture(program: &Path, args: &[&str], environment: &[(&str,&str)], seconds: u64) -> Result<Captured> {
    capture_in(program, args, environment, seconds, Path::new("/")).await
}

pub(super) async fn capture_in(program: &Path, args: &[&str], environment: &[(&str,&str)], seconds: u64, directory: &Path) -> Result<Captured> {
    let mut command = command(program, args, environment);
    let mut child = command.current_dir(directory).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let mut group = Group(child.id());
    let stdout = child.stdout.take().context("missing stdout")?;
    let stderr = child.stderr.take().context("missing stderr")?;
    let (stdout, stderr, status) = timeout(Duration::from_secs(seconds), async {
        tokio::try_join!(drain(stdout, MAX_CAPTURE), drain(stderr, 16 << 10), child.wait())
    }).await.context("context deadline exceeded")??;
    group.0 = None;
    ensure!(!stdout.truncated, "command output exceeds the 1 MiB capture limit");
    Ok(Captured { text: stdout.text(), stderr: stderr.text().trim().to_owned(), code: status.code() })
}

// Go Runner.Run sends stdout and stderr to the same bounded tail. A single
// socket pair preserves native interleaving and avoids unbounded child output.
pub(super) async fn run(program: &Path, args: &[&str], environment: &[(&str,&str)], seconds: u64, package: bool, output: &mut Tail) -> Result<Option<i32>> {
    let (reader, writer) = UnixStream::pair()?;
    reader.set_nonblocking(true)?;
    let reader = tokio::net::UnixStream::from_std(reader)?;
    let mut command = command(program, args, environment);
    let mut child = command
        .stdout(Stdio::from(OwnedFd::from(writer.try_clone()?)))
        .stderr(Stdio::from(OwnedFd::from(writer))).spawn()?;
    drop(command);
    let mut group = Group(child.id());
    let future = async { tokio::try_join!(drain(reader, MAX_TAIL), child.wait()) };
    tokio::pin!(future);
    let result = tokio::select! {
        result = &mut future => result,
        _ = tokio::time::sleep(Duration::from_secs(seconds)) => {
            if package {
                if let Some(pid) = group.0 { unsafe { libc::kill(-(pid as i32), libc::SIGINT); } }
                if let Ok(Ok((tail, _))) = timeout(Duration::from_secs(30), &mut future).await {
                    output.merge(tail, MAX_TAIL);
                    group.0 = None;
                }
            }
            anyhow::bail!("context deadline exceeded");
        }
    }?;
    group.0 = None;
    output.merge(result.0, MAX_TAIL);
    Ok(result.1.code())
}

const SYSTEMD_ENV: &[(&str,&str)] = &[("SYSTEMD_COLORS","0"),("SYSTEMD_URLIFY","0")];
const PROPERTIES: &str = "Id,Description,LoadState,ActiveState,SubState,UnitFileState,MainPID,Result,ExecMainCode,ExecMainStatus,ActiveEnterTimestamp,InactiveEnterTimestamp,FragmentPath,CanStart,CanStop,CanReload";

fn unit_valid(unit: &str) -> bool { crate::platforms::systemd::valid_unit(unit) }

fn service_row(name: &str) -> Value {
    json!({"name":name,"description":"","load":"unknown","active":"unknown","sub":"unknown","enabled":"unknown"})
}

fn parse_list(text: &str, files: bool) -> Result<Vec<Value>> {
    let mut rows = Vec::new();
    if text.trim_start().starts_with('[') {
        let native: Vec<Value> = serde_json::from_str(text)?;
        for row in native {
            let get = |key: &str| row.get(key).and_then(Value::as_str).unwrap_or_default();
            let name = get(if files {"unit_file"} else {"unit"});
            ensure!(unit_valid(name), "invalid service inventory row");
            if files {
                ensure!(!get("state").is_empty(), "invalid service inventory row");
                let mut item = json!({"name":name,"enabled":get("state")});
                if !get("preset").is_empty() { item["preset"] = json!(get("preset")); }
                rows.push(item);
            } else {
                ensure!(["load","active","sub"].iter().all(|key| !get(key).is_empty()), "invalid service inventory row");
                rows.push(json!({"name":name,"description":get("description"),"load":get("load"),"active":get("active"),"sub":get("sub")}));
            }
        }
    } else {
        for line in text.lines() {
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.is_empty() { continue; }
            ensure!(unit_valid(fields[0]), "invalid service inventory unit");
            if files {
                ensure!((2..=3).contains(&fields.len()), "invalid unit-file columns");
                let mut item = json!({"name":fields[0],"enabled":fields[1]});
                if fields.len() == 3 { item["preset"] = json!(fields[2]); }
                rows.push(item);
            } else {
                ensure!(fields.len() >= 4, "invalid loaded-unit columns");
                rows.push(json!({"name":fields[0],"description":"","load":fields[1],"active":fields[2],"sub":fields[3]}));
            }
        }
    }
    ensure!(rows.len() <= 4096, "service inventory exceeds 4096 units");
    Ok(rows)
}

async fn service(operation: &str, input: &Map<String,Value>, policy: &Policy, output: &mut Tail) -> OperationResult<Value> {
    let unit = string(input,"unit");
    if operation == "services_list" {
        let mut result = json!({"scope":"system","available":false,"journalAvailable":program("journalctl").is_some(),"services":[],"warnings":[]});
        let Some(systemctl) = program("systemctl") else {
            result["reason"] = json!("systemctl was not found; install systemd tools and restart server-man");
            return Ok(result);
        };
        result["available"] = json!(true);
        let mut merged = BTreeMap::new();
        let mut warnings = Vec::new();
        for action in ["list-unit-files","list-units"] {
            let mut args = vec!["--system","--no-pager","--no-ask-password",action,"--type=service","--full","--plain","--no-legend","--output=json"];
            if action == "list-units" { args.push("--all"); }
            let native = match capture(&systemctl,&args,SYSTEMD_ENV,12).await {
                Ok(native) if native.code == Some(0) => native,
                _ => { warnings.push(format!("{action} unavailable; check the configured system/user bus and permissions")); continue; }
            };
            let rows = match parse_list(&native.text,action == "list-unit-files") {
                Ok(rows) => rows,
                Err(_) => { warnings.push(format!("{action} returned unsupported or oversized inventory")); continue; }
            };
            for row in rows {
                let name = row["name"].as_str().unwrap_or_default();
                let item = merged.entry(name.to_owned()).or_insert_with(|| service_row(name));
                for (key,value) in row.as_object().expect("parsed row") {
                    item[key.as_str()] = value.clone();
                }
                if merged.len() > 4096 { return Err(Failure::unavailable("Service inventory exceeds 4096 units")); }
            }
        }
        result["services"] = json!(merged.into_values().collect::<Vec<_>>());
        result["warnings"] = json!(warnings);
        return Ok(result);
    }
    if !unit_valid(unit) {
        return Err(Failure::invalid(if operation == "services_details" {
            "Expected one exact .service unit name; patterns, paths and flags are unsupported"
        } else { "Invalid service unit name" }));
    }
    if operation == "services_logs" {
        let lines = input.get("tail").and_then(Value::as_f64).unwrap_or(200.0) as u64;
        let journal = program("journalctl").ok_or_else(|| Failure::unavailable("journalctl is unavailable"))?;
        let mut tail = Tail::default();
        let result = run(&journal,&["--system","--no-pager","--quiet","--output=short-iso","--unit",unit,"--lines",&lines.to_string()],SYSTEMD_ENV,10,false,&mut tail).await;
        if !matches!(result, Ok(Some(0))) {
            return Err(Failure::unavailable("Journal unavailable; check journal permissions and the configured scope"));
        }
        return Ok(json!({"text":tail.text(),"truncated":tail.truncated}));
    }
    let systemctl = program("systemctl").ok_or_else(|| Failure::unavailable("systemctl is unavailable"))?;
    if operation == "services_details" {
        let property = format!("--property={PROPERTIES}");
        let native = capture(&systemctl,&["--system","--no-pager","--no-ask-password","show",&property,"--",unit],SYSTEMD_ENV,12).await
            .ok().filter(|native| native.code == Some(0))
            .ok_or_else(|| Failure::unavailable("Service details unavailable; check the unit and configured system/user bus"))?;
        let fields: BTreeMap<_,_> = native.text.lines().filter_map(|line| line.split_once('=')).collect();
        let get = |key| fields.get(key).copied().unwrap_or_default();
        if get("LoadState") == "not-found" { return Err(Failure::new("not_found",404,"Service unit not found")); }
        if !unit_valid(get("Id")) || get("LoadState").is_empty() || get("ActiveState").is_empty() {
            return Err(Failure::unavailable("systemctl returned unsupported service details"));
        }
        let mut result = json!({"name":get("Id"),"description":get("Description"),"load":get("LoadState"),
            "active":get("ActiveState"),"sub":get("SubState"),"enabled":get("UnitFileState"),
            "canStart":get("CanStart")=="yes","canStop":get("CanStop")=="yes","canReload":get("CanReload")=="yes"});
        for (key,property) in [("result","Result"),("execMainCode","ExecMainCode"),("execMainStatus","ExecMainStatus"),("activeSince","ActiveEnterTimestamp"),("inactiveSince","InactiveEnterTimestamp"),("fragmentPath","FragmentPath")] {
            let value = get(property);
            if !value.is_empty() { result[key] = json!(value); }
        }
        if let Ok(pid) = get("MainPID").parse::<u64>() { result["mainPID"] = json!(pid); }
        return Ok(result);
    }
    // Maestro's approved unit policy is an additional deployment boundary;
    // do not relax it when moving to the reference request format.
    if !safe_system_unit(unit) || !policy.system_units.iter().any(|allowed| allowed == unit) {
        return Err(Failure::denied("system service is outside root-owned policy"));
    }
    let identity = capture(&systemctl,&["--system","--no-pager","--no-ask-password","show","--property=Id","--value","--",unit],SYSTEMD_ENV,12).await;
    if !identity.is_ok_and(|native| native.code == Some(0) && native.text.trim() == unit) {
        return Err(Failure::denied("systemd unit alias is outside policy"));
    }
    let action = operation.strip_prefix("services_").expect("service operation");
    let status = run(&systemctl,&["--system","--no-pager","--no-ask-password",action,"--",unit],SYSTEMD_ENV,1200,false,output).await
        .map_err(|err| Failure::failed(format!("Service {action} failed: {err}")))?;
    if status != Some(0) { return Err(Failure::failed(format!("Service {action} failed: systemctl: exit status {}",status.unwrap_or(-1)))); }
    Ok(json!({"scope":"system","unit":unit,"action":action}))
}

async fn password(input: &Map<String,Value>, policy: &Policy, uid: u32) -> OperationResult<Value> {
    let password = string(input,"password");
    if password.trim().is_empty() || password.len() > 1024 || password.contains(['\0','\r','\n']) {
        return Err(Failure::invalid("Samba password must be non-empty, at most 256 characters, and contain no NUL or line breaks"));
    }
    if !policy.samba_password { return Err(Failure::denied("Samba password administration is disabled by root policy")); }
    let account = admin_shares::caller_name(uid).map_err(|_| Failure::unavailable("The server-man Linux account is unavailable or unsupported"))?;
    if program("smbpasswd").is_none() { return Err(Failure::unavailable("smbpasswd is unavailable; set up the Samba server and restart server-man")); }
    admin_shares::set_password(uid,password).await.map_err(|_| Failure::unavailable("Could not set the Samba password; check Samba installation and privileged helper access"))?;
    Ok(json!({"username":account,"updated":true}))
}

use admin_packages::Family;
use crate::platforms::package_updates;

const PACKAGE_ENV: &[(&str,&str)] = &[("DEBIAN_FRONTEND","noninteractive"),("NEEDRESTART_MODE","l"),("NO_COLOR","1")];
const PACMAN_DB: &str = "/var/lib/maestro-admin/updates-pacman";

struct PackageBackend {
    family: Option<Family>,
    program: Option<PathBuf>,
    checkupdates: Option<PathBuf>,
    distribution: String,
    distribution_id: String,
    title: String,
    reason: String,
    refresh_reason: String,
    reboot_hint: bool,
}

fn native_family(id: &str) -> Option<Family> {
    match id {
        "debian" | "ubuntu" | "linuxmint" | "pop" => Some(Family::Apt),
        "fedora" | "rhel" | "centos" | "rocky" | "almalinux" | "ol" => Some(Family::Dnf),
        "arch" | "manjaro" | "endeavouros" => Some(Family::Pacman),
        "suse" | "opensuse" | "opensuse-leap" | "opensuse-tumbleweed" | "sles" | "sled" => Some(Family::Zypper),
        _ => None,
    }
}

impl PackageBackend {
    fn new() -> Self {
        use std::io::Read;
        let mut raw = String::new();
        let release = std::fs::File::open("/etc/os-release").and_then(|file| file.take(65537).read_to_string(&mut raw));
        if release.is_err() || raw.len() > 65536 { raw.clear(); }
        let values: BTreeMap<_,_> = raw.lines().filter_map(|line|line.split_once('='))
            .map(|(key,value)|(key,value.trim().trim_matches(['"','\'']))).collect();
        let get = |key| values.get(key).copied().unwrap_or_default();
        let checkupdates = program("checkupdates");
        let mut backend = Self {
            family: None, program: None, checkupdates,
            distribution:get("PRETTY_NAME").into(), distribution_id:get("ID").into(),
            title:String::new(), reason:String::new(), refresh_reason:String::new(), reboot_hint:false,
        };
        if Path::new("/run/ostree-booted").exists() || matches!(get("VARIANT_ID"),"silverblue"|"kinoite"|"sericea"|"onyx") {
            backend.title = "Image-based system".into();
            backend.reason = "Image-based OS updates are not supported; use the distribution's native update workflow".into();
            return backend;
        }
        let mut family = std::iter::once(get("ID")).chain(get("ID_LIKE").split_whitespace()).find_map(native_family);
        if family.is_none() {
            let found: Vec<_> = [(Family::Apt,"apt-get"),(Family::Dnf,"dnf"),(Family::Pacman,"pacman"),(Family::Zypper,"zypper")]
                .into_iter().filter(|(f,name)|program(name).is_some() || (*f==Family::Dnf && program("dnf5").is_some())).collect();
            if found.len()==1 { family=Some(found[0].0); }
        }
        backend.family=family;
        let Some(family)=family else {
            backend.title="No selected package manager".into();
            backend.reason="No unambiguous supported native manager detected; configure packageManager explicitly when appropriate".into();
            return backend;
        };
        let (title,name)=match family {
            Family::Apt=>("APT","apt-get"),
            Family::Dnf if program("dnf").is_none() && program("dnf5").is_some()=>("DNF 5","dnf5"),
            Family::Dnf=>("DNF","dnf"),
            Family::Pacman=>("Pacman","pacman"),
            Family::Zypper=>("Zypper","zypper"),
        };
        backend.title=title.into();
        backend.program=program(name);
        if backend.program.is_none() { backend.reason=format!("{name} executable was not found; install the native tools and restart server-man"); }
        if family==Family::Pacman && backend.checkupdates.is_none() {
            backend.refresh_reason="Install pacman-contrib/checkupdates for isolated metadata refresh. Install updates still uses a full pacman -Syu.".into();
        }
        backend
    }

    fn id(&self) -> &str { self.family.map_or("unsupported",Family::id) }

    fn reboot(&self) -> Value {
        let (required,reason)=match std::fs::metadata("/run/reboot-required") {
            Ok(_) => (Some(true),"The host's reboot-required marker is present"),
            Err(err) if err.kind()!=std::io::ErrorKind::NotFound => (None,"Reboot marker could not be inspected"),
            _ if self.reboot_hint => (Some(true),"Zypper reported that a reboot is required during this server process"),
            _ if self.family==Some(Family::Apt) => (Some(false),"No reboot requirement is declared by the Debian/Ubuntu marker"),
            _ => (None,"No reliable reboot requirement is available from this backend"),
        };
        json!({"required":required,"reason":reason})
    }

    async fn status(&self) -> Value {
        let mut manager=json!({"id":self.id(),"title":self.title,"available":self.program.is_some(),
            "canRefresh":self.program.is_some() && self.refresh_reason.is_empty()});
        if !self.reason.is_empty(){manager["reason"]=json!(self.reason);}
        if !self.refresh_reason.is_empty(){manager["refreshReason"]=json!(self.refresh_reason);}
        let mut result=json!({"manager":manager,"distribution":self.distribution,"packages":[],"valid":false,
            "notice":"Package listings use cached metadata. Refresh before reviewing updates; installation resolves the current repositories and may change the plan.",
            "cacheSource":"Native cached repository metadata","reboot":self.reboot(),"readAt":chrono::Utc::now()});
        let (Some(program),Some(family))=(&self.program,self.family) else {result["notice"]=json!(self.reason);return result;};
        let mut args=match family {
            Family::Apt=>package_updates::APT_ARGS,
            Family::Dnf=>package_updates::DNF_ARGS,
            Family::Pacman=>package_updates::PACMAN_ARGS,
            Family::Zypper=>package_updates::ZYPPER_ARGS,
        }.to_vec();
        if family==Family::Apt {
            result["notice"]=json!("Package listings use cached metadata. Refresh before reviewing updates; installation resolves the current repositories and may change the plan. APT holds/phased or dependency-blocked packages may remain deferred.");
        }
        if family==Family::Pacman && Path::new(PACMAN_DB).join("sync").is_dir(){
            args.extend(["--dbpath",PACMAN_DB]);result["cacheSource"]=json!("server-man's isolated checkupdates database");
        }
        let Ok(native)=capture(program,&args,&[("NO_COLOR","1")],12).await else {
            result["notice"]=json!("Pending updates are unavailable; check package-manager permissions, cached metadata and native diagnostics");return result;
        };
        if !(native.code==Some(0) || family==Family::Zypper && matches!(native.code,Some(100|101))
            || family==Family::Pacman && native.code==Some(1) && native.text.trim().is_empty() && native.stderr.is_empty()){
            result["notice"]=json!("Pending updates are unavailable; check package-manager permissions, cached metadata and native diagnostics");return result;
        }
        if let Some(rows)=package_updates::reference_package_rows(family,&native.text){
            result["packages"]=rows;result["valid"]=json!(true);
        }else{result["notice"]=json!("Package manager returned unsupported or oversized update data");}
        result
    }

    async fn refresh(&self,output:&mut Tail)->OperationResult<Value>{
        let native=self.program.as_ref().ok_or_else(||Failure::unavailable(&self.reason))?;
        let family=self.family.ok_or_else(||Failure::unavailable("Unsupported package manager"))?;
        let arguments=admin_packages::refresh_args(family);
        let arguments:Vec<_>=arguments.iter().map(String::as_str).collect();
        let mut environment=PACKAGE_ENV.to_vec();
        let program=if family==Family::Pacman{
            let program=self.checkupdates.as_ref().ok_or_else(||Failure::unavailable(&self.refresh_reason))?;
            prepare_pacman_cache().map_err(|_|Failure::unavailable("A private absolute state directory is required for checkupdates"))?;
            environment.push(("CHECKUPDATES_DB",PACMAN_DB));program
        }else{native};
        let code=run(program,&arguments,&environment,1200,true,output).await
            .map_err(|err|Failure::failed(format!("Metadata refresh failed: {err}")))?;
        if code!=Some(0) && !(family==Family::Pacman && code==Some(2)){
            return Err(Failure::failed(format!("Metadata refresh failed: {}: exit status {}",program.file_name().unwrap_or_default().to_string_lossy(),code.unwrap_or(-1))));
        }
        Ok(json!({"manager":self.id(),"action":"metadata refreshed"}))
    }

    async fn install(&mut self, packages:Option<Vec<String>>, output:&mut Tail)->OperationResult<Value>{
        let program=self.program.as_ref().ok_or_else(||Failure::unavailable(&self.reason))?;
        let family=self.family.ok_or_else(||Failure::unavailable("unsupported package manager"))?;
        if family!=Family::Pacman { self.refresh(output).await?; }
        let args=if let Some(names)=&packages{admin_packages::install_args(family,names)}else{admin_packages::upgrade_args(family)};
        let args:Vec<_>=args.iter().map(String::as_str).collect();
        let code=run(program,&args,PACKAGE_ENV,1200,true,output).await.map_err(|err|Failure::failed(format!("Package installation failed: {err}")))?;
        let recheck=packages.is_none() && family==Family::Zypper && code==Some(103);
        if packages.is_none() && family==Family::Zypper && code==Some(102){self.reboot_hint=true;}
        if code!=Some(0) && !(packages.is_none() && family==Family::Zypper && matches!(code,Some(102|103))){
            return Err(Failure::failed(format!("Package installation failed: {}: exit status {}",program.file_name().unwrap_or_default().to_string_lossy(),code.unwrap_or(-1))));
        }
        Ok(if let Some(packages)=packages{json!({"manager":self.id(),"action":"packages installed","packages":packages})}
            else{json!({"manager":self.id(),"action":"updates installed","reboot":self.reboot(),"recheckRequired":recheck})})
    }
}

fn prepare_pacman_cache()->Result<()>{
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    for path in ["/var/lib/maestro-admin",PACMAN_DB]{
        match std::fs::DirBuilder::new().mode(0o700).create(path){
            Ok(())=>{},Err(err) if err.kind()==std::io::ErrorKind::AlreadyExists=>{},Err(err)=>return Err(err.into())
        }
        let metadata=std::fs::symlink_metadata(path)?;
        ensure!(metadata.is_dir() && metadata.uid()==0 && metadata.mode()&0o077==0,"unsafe private Pacman cache");
    }
    Ok(())
}

async fn packages(operation:&str,input:&Map<String,Value>,policy:&Policy,output:&mut Tail)->OperationResult<Value>{
    let mut backend=PackageBackend::new();
    if operation=="updates_status"{return Ok(backend.status().await);}
    if !policy.package_writes{return Err(Failure::denied("Package writes are disabled by root-owned policy"));}
    match operation{
        "updates_refresh"=>backend.refresh(output).await,
        "updates_install"=>backend.install(None,output).await,
        "packages_install"=>{
            let names=admin_packages::package_names(string(input,"packages")).map_err(|error| {
                let message=error.to_string();
                Failure::invalid(if message=="invalid or duplicate native repository package name"{"invalid or duplicate repository package name".into()}else{message})
            })?;
            backend.install(Some(names),output).await
        }
        "dependencies_install"=>{
            let family=backend.family.ok_or_else(||Failure::invalid("unsupported dependency target or package manager"))?;
            let names=admin_packages::dependency_packages(family,&backend.distribution_id,string(input,"target"))
                .map_err(|_|Failure::invalid("unsupported dependency target or package manager"))?;
            backend.install(Some(names),output).await
        }
        _=>Err(Failure::invalid("unknown operation")),
    }
}

pub(super) async fn execute(request: Request, policy: &Policy, uid: u32) -> Response {
    let mut output = Tail::default();
    let result = match validate(&request) {
        Err(error) => Err(error),
        Ok(input) => match request.operation.as_str() {
            operation if operation.starts_with("services_") => service(operation,input,policy,&mut output).await,
            "shares_samba_password" => password(input,policy,uid).await,
            "updates_status" | "updates_refresh" | "updates_install" | "packages_install" | "dependencies_install" => packages(&request.operation,input,policy,&mut output).await,
            "shares_list" | "shares_setup" | "shares_definitions_inspect" | "shares_definitions_validate" | "shares_definitions_apply" => Err(Failure::unavailable("Native reference share operation has not yet been ported")),
            _ => Err(Failure::invalid("unknown operation")),
        },
    };
    let mut response = Response { output:output.text(),truncated:output.truncated,..Default::default() };
    match result {
        Ok(result) => response.result = Some(result),
        Err(error) => { response.error = error.message; response.code = error.code.into(); response.status = error.status; }
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reference_request_rejections() {
        for (input,error) in [
            ("{}", "operation is not allowed"),
            (r#"{"operation":"compose_up"}"#, "operation is not allowed"),
            (r#"{"operation":"services_start","extra":true}"#, "invalid helper request"),
            (r#"{"operation":"services_start"} {}"#, "expected one helper request"),
            (r#"{"operation":"services_start","arguments":{"scope":"user"}}"#, "helper manages system services only"),
        ] {
            assert_eq!(decode(input.as_bytes()).err().unwrap().to_string(),error);
        }
        assert_eq!(decode(&vec![b'x'; MAX_REQUEST+1]).err().unwrap().to_string(),"request exceeds 1 MiB");
    }
    #[test]
    fn reference_argument_errors_and_response_shape() {
        for (input,error) in [
            (r#"{"operation":"services_stop","arguments":{"unit":"x.service","confirm":false}}"#,"confirm must be true for this operation"),
            (r#"{"operation":"services_stop","arguments":{"unit":"x.service","confirm":true,"command":"sh"}}"#,"unknown argument \"command\""),
            (r#"{"operation":"shares_samba_password","arguments":{"password":"","confirm":true}}"#,"password is too short"),
            (r#"{"operation":"updates_status","arguments":null}"#,"arguments must be an object"),
        ] {
            let request = decode(input.as_bytes()).unwrap();
            assert_eq!(validate(&request).unwrap_err().message,error);
        }
        let response = Response { result:Some(json!({"scope":"system","unit":"x.service","action":"start"})),..Default::default() };
        assert_eq!(serde_json::to_value(response).unwrap(),json!({"result":{"scope":"system","unit":"x.service","action":"start"}}));
    }
}
