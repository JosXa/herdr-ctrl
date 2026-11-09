//! Errors with a stable kind, so a calling agent can branch on them.

use std::fmt;

#[derive(Debug)]
pub struct Fail {
    pub kind: &'static str,
    pub message: String,
    pub hint: Option<String>,
}

impl Fail {
    pub fn new(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            hint: None,
        }
    }

    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn exit_code(&self) -> i32 {
        match self.kind {
            "usage" => 2,
            "not_found" | "stale_ref" | "ambiguous" | "covered" | "not_visible" => 3,
            "timeout" => 4,
            "assertion" => 5,
            "no_instance" | "no_ctrl" => 6,
            _ => 1,
        }
    }
}

impl fmt::Display for Fail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for Fail {}

pub fn fail(kind: &'static str, message: impl Into<String>) -> anyhow::Error {
    Fail::new(kind, message).into()
}
