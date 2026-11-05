use crate::config::{Config, DEFAULT_LEAD_MAX_PEERS, ROLE_NAMES};
use crate::layout::model::{Layout, LayoutRole, Naming, RoleClass};
use std::collections::BTreeMap;

impl Layout {
    /// Synthesize an SLP layout from a legacy v2 Config.
    ///
    /// The mapping covers:
    /// - `can_spawn` and `can_prompt`, copied as they are;
    /// - `reports_to`;
    /// - `[limits].depth`;
    /// - `roles.lead.max_peers` -> `lead.max_children`;
    /// - `limits.max_workers_per_orchestrator` -> `cos.max_children`.
    pub fn from_legacy(cfg: &Config) -> Self {
        let mut roles = BTreeMap::new();
        for name in &ROLE_NAMES {
            let name_str = name.to_string();
            let role_cfg = cfg.roles.get(&name_str);
            let class = match *name {
                "cos" => RoleClass::Cos,
                "lead" => RoleClass::Lead,
                "peer" => RoleClass::Peer,
                _ => RoleClass::Peer,
            };
            let can_spawn = match role_cfg {
                Some(r) => r.can_spawn.clone(),
                None => match *name {
                    "cos" => vec![
                        "lead".to_string(),
                        "peer".to_string(),
                        "taskforce".to_string(),
                    ],
                    "lead" => vec!["peer".to_string(), "taskforce".to_string()],
                    _ => Vec::new(),
                },
            };
            let can_prompt = match role_cfg {
                Some(r) => r.can_prompt.clone(),
                None => match *name {
                    "cos" => vec!["lead".to_string(), "peer".to_string()],
                    "lead" => vec!["lead".to_string(), "peer".to_string()],
                    _ => Vec::new(),
                },
            };
            let reports_to = match role_cfg.and_then(|r| r.reports_to.clone()) {
                Some(r) => Some(r),
                None => match *name {
                    "lead" => Some("cos".to_string()),
                    "peer" => Some("owner".to_string()),
                    _ => None,
                },
            };
            let max_children = match *name {
                "lead" => {
                    let peers = role_cfg
                        .and_then(|r| r.max_peers)
                        .unwrap_or(DEFAULT_LEAD_MAX_PEERS);
                    Some(peers)
                }
                "cos" => Some(cfg.limits.max_workers_per_orchestrator),
                _ => None,
            };
            let naming = match *name {
                "lead" => Naming::SlugLead,
                _ => Naming::KindTask,
            };
            roles.insert(
                name_str,
                LayoutRole {
                    class,
                    can_spawn,
                    can_prompt,
                    reports_to,
                    max_children,
                    naming,
                    members: Vec::new(),
                    quorum: None,
                },
            );
        }

        // SLP also defines taskforce as a spawnable target with reports_to = "owner"
        roles.insert(
            "taskforce".to_string(),
            LayoutRole {
                class: RoleClass::Taskforce,
                can_spawn: Vec::new(),
                can_prompt: Vec::new(),
                reports_to: Some("owner".to_string()),
                max_children: None,
                naming: Naming::KindTask,
                members: Vec::new(),
                quorum: None,
            },
        );

        Self {
            name: "slp".to_string(),
            depth: cfg.limits.depth.max(0) as u32,
            roles,
        }
    }
}
