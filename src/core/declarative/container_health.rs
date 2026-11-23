use super::Container;
use anyhow::{Result, ensure};
use std::collections::BTreeMap;

pub(super) fn validate(containers: &BTreeMap<String, Container>) -> Result<()> {
    for (name, container) in containers {
        ensure!(
            container.depends_on.len() <= 32,
            "container dependency limit exceeded"
        );
        for (dependency, condition) in &container.depends_on {
            let upstream = containers.get(dependency);
            ensure!(
                dependency != name
                    && upstream.is_some()
                    && matches!(condition.as_str(), "started" | "healthy"),
                "invalid container dependency"
            );
            ensure!(
                condition != "healthy"
                    || upstream.is_some_and(|upstream| upstream.healthcheck.is_some()),
                "healthy dependency requires an upstream healthcheck"
            );
        }
        if let Some(health) = &container.healthcheck {
            ensure!(
                (2..=32).contains(&health.test.len())
                    && matches!(health.test[0].as_str(), "CMD" | "CMD-SHELL"),
                "invalid healthcheck command"
            );
            ensure!(
                health.test[0] != "CMD-SHELL" || health.test.len() == 2,
                "healthcheck shell command must have one argument"
            );
            ensure!(
                health.test[1..]
                    .iter()
                    .all(|arg| arg.len() <= 8192 && !arg.chars().any(char::is_control)),
                "invalid healthcheck argument"
            );
            ensure!(
                (0..=1000).contains(&health.retries)
                    && [&health.interval, &health.timeout, &health.start_period]
                        .into_iter()
                        .all(|value| duration(value)),
                "invalid healthcheck timing"
            );
        }
    }
    Ok(())
}

// Positive Go time.ParseDuration syntax, bounded to its signed nanosecond range.
fn duration(value: &str) -> bool {
    if value.is_empty() {
        return true;
    }
    let mut remaining = value.strip_prefix('+').unwrap_or(value);
    let mut total = 0_u128;
    while !remaining.is_empty() {
        let length = remaining
            .bytes()
            .take_while(|byte| byte.is_ascii_digit() || *byte == b'.')
            .count();
        if length == 0 {
            return false;
        }
        let number = &remaining[..length];
        remaining = &remaining[length..];
        let unit_end = remaining
            .bytes()
            .position(|byte| byte.is_ascii_digit() || byte == b'.')
            .unwrap_or(remaining.len());
        let unit = match &remaining[..unit_end] {
            "ns" => 1_u128,
            "us" | "µs" | "μs" => 1_000,
            "ms" => 1_000_000,
            "s" => 1_000_000_000,
            "m" => 60_000_000_000,
            "h" => 3_600_000_000_000,
            _ => return false,
        };
        remaining = &remaining[unit_end..];
        let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
        if whole.is_empty() && fraction.is_empty() {
            return false;
        }
        if !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
            return false;
        }
        let Ok(whole) = (if whole.is_empty() {
            Ok(0)
        } else {
            whole.parse::<u128>()
        }) else {
            return false;
        };
        let Some(whole) = whole.checked_mul(unit) else {
            return false;
        };
        let fraction = &fraction[..fraction.len().min(18)];
        let digits = fraction.parse::<u128>().unwrap_or(0);
        let Some(next) = total.checked_add(whole).and_then(|value| {
            value.checked_add(digits * unit / 10_u128.pow(fraction.len() as u32))
        }) else {
            return false;
        };
        total = next;
        if total > i64::MAX as u128 {
            return false;
        }
    }
    total > 0
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn durations_match_captured_go_contract() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../test-fixtures/phase2/health-durations.json"
        ))
        .unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let value = case["value"].as_str().unwrap();
            assert_eq!(duration(value), case["valid"].as_bool().unwrap(), "{value}");
        }
    }

    #[test]
    fn invalid_health_commands_and_dependency_conditions_fail_before_publication() {
        use crate::core::declarative::ContainerHealth;
        let mut containers = BTreeMap::from([(
            "db".into(),
            Container {
                image: "postgres:16".into(),
                ..Default::default()
            },
        )]);
        for health in [
            ContainerHealth {
                test: vec!["NONE".into()],
                ..Default::default()
            },
            ContainerHealth {
                test: vec!["CMD-SHELL".into(), "true".into(), "extra".into()],
                ..Default::default()
            },
            ContainerHealth {
                test: vec!["CMD".into(), "bad\nargument".into()],
                ..Default::default()
            },
            ContainerHealth {
                test: vec!["CMD".into(), "true".into()],
                retries: 1001,
                ..Default::default()
            },
        ] {
            containers.get_mut("db").unwrap().healthcheck = Some(health);
            assert!(validate(&containers).is_err());
        }
        containers.get_mut("db").unwrap().healthcheck = None;
        containers.insert(
            "app".into(),
            Container {
                depends_on: BTreeMap::from([("db".into(), "healthy".into())]),
                ..Default::default()
            },
        );
        assert!(validate(&containers).is_err());
        containers
            .get_mut("app")
            .unwrap()
            .depends_on
            .insert("db".into(), "unknown".into());
        assert!(validate(&containers).is_err());
    }
    #[test]
    fn duration_contract_matches_positive_reference_durations() {
        for value in [
            "",
            "10s",
            "+.5s",
            "1h30m",
            "1.s",
            "1µs",
            "1μs",
            "0.000000001s",
            "9223372036854775807ns",
        ] {
            assert!(duration(value), "{value}");
        }
        for value in [
            "0",
            "0s",
            "-1s",
            "tomorrow",
            "1d",
            "1..2s",
            ".s",
            "+",
            "1s ",
            "1s-2s",
            "0.0000000001s",
            "9223372036854775808ns",
            "99999999999999999999999999999999999999999h",
        ] {
            assert!(!duration(value), "{value}");
        }
    }
}
