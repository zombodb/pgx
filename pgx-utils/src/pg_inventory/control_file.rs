use core::convert::TryFrom;
use std::collections::HashMap;
use tracing_error::SpanTrace;

#[derive(Debug, Clone, Hash, PartialOrd, Ord, PartialEq, Eq)]
pub struct ControlFile {
    pub comment: String,
    pub default_version: String,
    pub module_pathname: String,
    pub relocatable: bool,
    pub superuser: bool,
    pub schema: Option<String>,
}

#[derive(Debug, Clone)]
pub enum ControlFileError {
    MissingField {
        field: &'static str,
        context: SpanTrace,
    },
}

impl std::fmt::Display for ControlFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ControlFileError::MissingField { field, context } => {
                write!(f, "Missing field in control file! Please add `{}`.", field)?;
                context.fmt(f)?;
            }
        };
        Ok(())
    }
}

impl std::error::Error for ControlFileError {}

impl TryFrom<&str> for ControlFile {
    type Error = ControlFileError;

    fn try_from(input: &str) -> Result<Self, Self::Error> {
        let mut temp = HashMap::new();
        for line in input.lines() {
            let parts: Vec<&str> = line.split('=').collect();

            if parts.len() != 2 {
                continue;
            }

            let (k, v) = (parts.get(0).unwrap().trim(), parts.get(1).unwrap().trim());

            let v = v.trim_start_matches('\'');
            let v = v.trim_end_matches('\'');

            temp.insert(k, v);
        }
        Ok(ControlFile {
            comment: temp
                .get("comment")
                .ok_or(ControlFileError::MissingField {
                    field: "comment",
                    context: SpanTrace::capture(),
                })?
                .to_string(),
            default_version: temp
                .get("default_version")
                .ok_or(ControlFileError::MissingField {
                    field: "default_version",
                    context: SpanTrace::capture(),
                })?
                .to_string(),
            module_pathname: temp
                .get("module_pathname")
                .ok_or(ControlFileError::MissingField {
                    field: "module_pathname",
                    context: SpanTrace::capture(),
                })?
                .to_string(),
            relocatable: temp
                .get("relocatable")
                .ok_or(ControlFileError::MissingField {
                    field: "relocatable",
                    context: SpanTrace::capture(),
                })?
                == &"true",
            superuser: temp
                .get("superuser")
                .ok_or(ControlFileError::MissingField {
                    field: "superuser",
                    context: SpanTrace::capture(),
                })?
                == &"true",
            schema: temp.get("schema").map(|v| v.to_string()),
        })
    }
}
