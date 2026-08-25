//! Strict plugin-configuration parsing shared by execution and completion.

use nu_protocol::{Span, Value};

use super::CallerEngine;
use crate::domain::{AgentIdlePolicy, AgentStatus, Error};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CommandConfig {
    pub(crate) dynamic_completion: bool,
    pub(crate) idle_agent_policy: AgentIdlePolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConfigurationError {
    pub(crate) error: Error,
    pub(crate) span: Span,
}

impl ConfigurationError {
    fn new(message: impl Into<String>, span: Span) -> Self {
        Self {
            error: Error::invalid_configuration(message),
            span,
        }
    }
}

pub(crate) fn read_command_config(
    engine: &impl CallerEngine,
    head: Span,
) -> Result<CommandConfig, ConfigurationError> {
    let config = engine
        .plugin_config()
        .map_err(|_| ConfigurationError::new("plugin configuration is unavailable", head))?;
    parse_plugin_config(config.as_ref())
}

pub(crate) fn parse_plugin_config(
    config: Option<&Value>,
) -> Result<CommandConfig, ConfigurationError> {
    let Some(value) = config else {
        return Ok(CommandConfig::default());
    };
    let Value::Record { val: record, .. } = value else {
        return Err(ConfigurationError::new(
            "plugin configuration must be a record",
            value.span(),
        ));
    };

    if let Some((key, _)) = record
        .iter()
        .find(|(key, _)| !matches!(key.as_str(), "dynamic_completion" | "idle_agent_statuses"))
    {
        return Err(ConfigurationError::new(
            format!("unknown plugin configuration key: {key}"),
            value.span(),
        ));
    }

    let dynamic_completion = matches!(
        record.get("dynamic_completion"),
        Some(Value::Bool { val: true, .. })
    );
    let idle_agent_policy = match record.get("idle_agent_statuses") {
        None => AgentIdlePolicy::default(),
        Some(statuses) => parse_idle_agent_statuses(statuses)?,
    };
    Ok(CommandConfig {
        dynamic_completion,
        idle_agent_policy,
    })
}

fn parse_idle_agent_statuses(value: &Value) -> Result<AgentIdlePolicy, ConfigurationError> {
    let Value::List { vals, .. } = value else {
        return Err(ConfigurationError::new(
            "idle_agent_statuses must be a list",
            value.span(),
        ));
    };
    let statuses = vals
        .iter()
        .map(|value| {
            let Value::String { val, .. } = value else {
                return Err(ConfigurationError::new(
                    "idle_agent_statuses entries must be strings",
                    value.span(),
                ));
            };
            match val.as_str() {
                "idle" => Ok(AgentStatus::Idle),
                "done" => Ok(AgentStatus::Done),
                "blocked" => Ok(AgentStatus::Blocked),
                "working" => Ok(AgentStatus::Working),
                _ => Err(ConfigurationError::new(
                    format!("unsupported idle agent status: {val}"),
                    value.span(),
                )),
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(AgentIdlePolicy::from_statuses(statuses))
}

#[cfg(test)]
mod tests {
    use super::parse_plugin_config;
    use crate::domain::{AgentStatus, ErrorKind};
    use nu_protocol::{Span, Value, record};

    fn sp(start: usize) -> Span {
        Span::new(start, start + 1)
    }

    fn config_with_statuses(statuses: Vec<Value>) -> Value {
        Value::record(
            record! {
                "idle_agent_statuses" => Value::list(statuses, sp(20)),
            },
            sp(10),
        )
    }

    #[test]
    fn missing_config_and_missing_status_key_use_backward_compatible_defaults() {
        for config in [None, Some(Value::record(record! {}, sp(10)))] {
            let parsed = parse_plugin_config(config.as_ref()).unwrap();
            assert!(parsed.idle_agent_policy.allows(AgentStatus::Idle));
            assert!(parsed.idle_agent_policy.allows(AgentStatus::Done));
            assert!(!parsed.idle_agent_policy.allows(AgentStatus::Blocked));
            assert!(!parsed.idle_agent_policy.allows(AgentStatus::Working));
            assert!(!parsed.idle_agent_policy.allows(AgentStatus::Unknown));
        }
    }

    #[test]
    fn accepted_statuses_have_set_semantics_and_empty_is_valid() {
        let cases = [
            (vec![], vec![]),
            (vec!["idle"], vec![AgentStatus::Idle]),
            (vec!["done"], vec![AgentStatus::Done]),
            (vec!["blocked"], vec![AgentStatus::Blocked]),
            (vec!["working"], vec![AgentStatus::Working]),
            (
                vec!["working", "idle", "working", "done", "blocked"],
                vec![
                    AgentStatus::Idle,
                    AgentStatus::Done,
                    AgentStatus::Blocked,
                    AgentStatus::Working,
                ],
            ),
        ];
        for (configured, expected) in cases {
            let value = config_with_statuses(
                configured
                    .iter()
                    .copied()
                    .map(|status| Value::string(status, sp(30)))
                    .collect(),
            );
            let parsed = parse_plugin_config(Some(&value)).unwrap();
            for status in [
                AgentStatus::Idle,
                AgentStatus::Done,
                AgentStatus::Blocked,
                AgentStatus::Working,
            ] {
                assert_eq!(
                    parsed.idle_agent_policy.allows(status),
                    expected.contains(&status),
                    "unexpected membership for {status:?} in {configured:?}"
                );
            }
            assert!(!parsed.idle_agent_policy.allows(AgentStatus::Unknown));
        }
    }

    #[test]
    fn only_boolean_true_enables_dynamic_completion_without_affecting_validation() {
        let cases = [
            (None, false),
            (Some(Value::bool(false, sp(30))), false),
            (Some(Value::string("true", sp(30))), false),
            (Some(Value::int(1, sp(30))), false),
            (Some(Value::bool(true, sp(30))), true),
        ];
        for (value, expected) in cases {
            let config =
                value.map(|value| Value::record(record! { "dynamic_completion" => value }, sp(10)));
            let parsed = parse_plugin_config(config.as_ref()).unwrap();
            assert_eq!(parsed.dynamic_completion, expected);
        }
    }

    #[test]
    fn malformed_values_report_the_most_specific_span() {
        let cases = [
            (Value::bool(true, sp(1)), sp(1)),
            (
                Value::record(
                    record! { "idle_agent_statuses" => Value::string("idle", sp(2)) },
                    sp(10),
                ),
                sp(2),
            ),
            (config_with_statuses(vec![Value::int(1, sp(3))]), sp(3)),
        ];
        for (value, expected_span) in cases {
            let error = parse_plugin_config(Some(&value)).unwrap_err();
            assert_eq!(error.error.kind(), ErrorKind::InvalidConfiguration);
            assert_eq!(error.span, expected_span);
        }
    }

    #[test]
    fn unsupported_status_spellings_are_rejected_at_the_member_span() {
        for status in ["unknown", "busy", "Idle", "DONE", " idle", "done "] {
            let member_span = sp(40);
            let value = config_with_statuses(vec![Value::string(status, member_span)]);
            let error = parse_plugin_config(Some(&value)).unwrap_err();
            assert_eq!(error.error.kind(), ErrorKind::InvalidConfiguration);
            assert_eq!(error.span, member_span, "{status}");
        }
    }

    #[test]
    fn unknown_keys_are_rejected_at_the_record_span() {
        for key in ["other", "idle_agent_status", "Dynamic_completion"] {
            let record_span = sp(50);
            let mut record = nu_protocol::Record::new();
            record.push(key, Value::nothing(sp(51)));
            let value = Value::record(record, record_span);
            let error = parse_plugin_config(Some(&value)).unwrap_err();
            assert_eq!(error.error.kind(), ErrorKind::InvalidConfiguration);
            assert_eq!(error.span, record_span, "{key}");
        }
    }
}
