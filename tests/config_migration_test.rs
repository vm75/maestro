use std::{fs, process::Command};

#[test]
fn one_time_cli_migration_and_yaml_runtime_paths() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("maestro.toml");
    let original = format!(
        "# preserve this exact backup\ntest_mode = true\n[server]\nauth_token = 'migration-test-token'\n[paths]\ndata_dir = '{}'\n[nginx]\nroot_dir = '{}'\ninputs_dir = '{}'\n",
        root.path().join("state").display(),
        root.path().join("nginx").display(),
        root.path().join("inputs").display()
    );
    fs::write(&source, &original).unwrap();
    let binary = env!("CARGO_BIN_EXE_maestro");
    let migrate = || {
        Command::new(binary)
            .args(["--config", source.to_str().unwrap(), "migrate-config"])
            .output()
            .unwrap()
    };
    let output = migrate();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let target = source.with_extension("yaml");
    assert_eq!(
        fs::read_to_string(source.with_extension("toml.bak")).unwrap(),
        original
    );
    assert!(!source.exists());
    assert!(migrate().status.success());
    let check = Command::new(binary)
        .args(["--config", target.to_str().unwrap(), "check-config"])
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    let value = Command::new(binary)
        .args([
            "--config",
            target.to_str().unwrap(),
            "config-value",
            "server.auth_token",
        ])
        .output()
        .unwrap();
    assert!(value.status.success());
    assert_eq!(
        String::from_utf8(value.stdout).unwrap().trim(),
        "migration-test-token"
    );
    let old = Command::new(binary)
        .args(["--config", source.to_str().unwrap(), "check-config"])
        .output()
        .unwrap();
    assert!(!old.status.success());
}
