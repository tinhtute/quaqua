use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layout {
    #[serde(default)]
    pub name: String,
    pub depth: u32,
    #[serde(default)]
    pub roles: BTreeMap<String, LayoutRole>,
}

impl Layout {
    pub fn edge_set(&self) -> BTreeSet<(String, String, String)> {
        let mut edges = BTreeSet::new();
        for (from, role) in &self.roles {
            for target in &role.can_spawn {
                edges.insert((from.clone(), "spawn".to_string(), target.clone()));
            }
            for target in &role.can_prompt {
                edges.insert((from.clone(), "prompt".to_string(), target.clone()));
            }
            if let Some(target) = &role.reports_to {
                edges.insert((from.clone(), "reports_to".to_string(), target.clone()));
            }
        }
        edges
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayoutRole {
    pub class: RoleClass,
    #[serde(default)]
    pub can_spawn: Vec<String>,
    #[serde(default)]
    pub can_prompt: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reports_to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_children: Option<i64>,
    #[serde(default)]
    pub naming: Naming,
    #[serde(default)]
    pub members: Vec<Member>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quorum: Option<usize>,
}

impl LayoutRole {
    pub fn effective_quorum(&self) -> Option<usize> {
        if self.members.is_empty() {
            None
        } else {
            Some(self.quorum.unwrap_or_else(|| self.members.len() / 2 + 1))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RoleClass {
    Cos,
    Lead,
    Peer,
    Taskforce,
}

impl RoleClass {
    pub fn as_str(&self) -> &'static str {
        match self {
            RoleClass::Cos => "cos",
            RoleClass::Lead => "lead",
            RoleClass::Peer => "peer",
            RoleClass::Taskforce => "taskforce",
        }
    }
}

impl fmt::Display for RoleClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[derive(Default)]
pub enum Naming {
    SlugLead,
    #[default]
    KindTask,
}

impl Naming {
    pub fn as_str(&self) -> &'static str {
        match self {
            Naming::SlugLead => "slug-lead",
            Naming::KindTask => "kind-task",
        }
    }
}

impl fmt::Display for Naming {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Member {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verb {
    Spawn,
    Prompt,
    Mail,
}

impl Verb {
    pub fn as_str(&self) -> &'static str {
        match self {
            Verb::Spawn => "spawn",
            Verb::Prompt => "prompt",
            Verb::Mail => "mail",
        }
    }
}

impl fmt::Display for Verb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl FromStr for Verb {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "spawn" => Ok(Verb::Spawn),
            "prompt" => Ok(Verb::Prompt),
            "mail" => Ok(Verb::Mail),
            other => Err(format!(
                "unknown verb '{other}' (expected 'spawn', 'prompt', or 'mail')"
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TraceStep {
    pub layer: String,
    pub passed: bool,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Decision {
    pub allow: bool,
    pub rule_id: String,
    pub trace: Vec<TraceStep>,
}

impl Decision {
    pub fn allow(rule_id: impl Into<String>, trace: Vec<TraceStep>) -> Self {
        Self {
            allow: true,
            rule_id: rule_id.into(),
            trace,
        }
    }

    pub fn deny(rule_id: impl Into<String>, trace: Vec<TraceStep>) -> Self {
        Self {
            allow: false,
            rule_id: rule_id.into(),
            trace,
        }
    }
}
