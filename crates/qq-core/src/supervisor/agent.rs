//! The short-lived Supervisor agent (R13): read-only. It may read a run, read
//! files, read zbrain, look code up, put an evidence-backed question to the
//! Lead and file one six-field proposal. It cannot write code, give an order
//! or accept anything. It has no skill set and no handle on the backlog's
//! verdicts: [`AgentRunner`] returns an [`Answer`] and the job files it.

use crate::backlog::{Change, Text};
use crate::kb::{self, Caller};
use crate::supervisor::rubric::Rubric;

/// What the job found: one rubric scored 0 on one run, with the quoted evidence.
#[derive(Clone, Debug)]
pub struct Finding {
    pub run_id: i64,
    pub project_id: Option<i64>,
    pub rubric: Rubric,
    pub quote: String,
}

/// The agent's result: the six proposal fields and what the change would touch
/// (the backlog derives the risk from it).
#[derive(Clone, Debug)]
pub struct Answer {
    pub text: Text,
    pub change: Change,
}

impl Answer {
    /// All six fields must be there and not blank.
    pub fn check(&self) -> Result<(), String> {
        let t = &self.text;
        let fields = [
            ("observation", &t.observation),
            ("evidence", &t.evidence),
            ("suspected mechanism", &t.suspected_mechanism),
            ("impact", &t.impact),
            ("question", &t.question),
            ("recommendation", &t.recommendation),
        ];
        for (name, v) in fields {
            if v.as_deref().map(str::trim).unwrap_or("").is_empty() {
                return Err(format!("proposal field '{name}' is empty"));
            }
        }
        Ok(())
    }
}

pub trait AgentRunner {
    fn investigate(&self, finding: &Finding) -> Result<Answer, String>;
}

/// Everything the agent might try. Matching is exhaustive on purpose: a new
/// action must be classified here before it compiles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentAction {
    ReadRun,
    ReadFile,
    ReadZbrain,
    CodeLookup,
    QuestionLead,
    FileProposal,
    WriteFile,
    EditCode,
    SendOrder,
    Accept,
    Spawn,
    Merge,
    Push,
    WriteZbrain,
}

#[derive(Debug, PartialEq, Eq)]
pub struct AgentRefusal(pub String);

impl std::fmt::Display for AgentRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

pub fn permit(a: AgentAction) -> Result<(), AgentRefusal> {
    use AgentAction::*;
    match a {
        ReadRun | ReadFile | ReadZbrain | CodeLookup | QuestionLead | FileProposal => Ok(()),
        WriteFile | EditCode | WriteZbrain => Err(AgentRefusal(
            "the Supervisor agent is read-only: it writes no code and no file".into(),
        )),
        SendOrder | Spawn | Merge | Push => Err(AgentRefusal(
            "the Supervisor agent gives no order and runs no orchestration call".into(),
        )),
        Accept => Err(AgentRefusal(
            "the Supervisor agent accepts nothing: acceptance is the Lead's, the CoS's and anh iu's".into(),
        )),
    }
}

/// The tools the agent's CLI is given: read tools only.
pub fn agent_tools() -> Vec<String> {
    let mut tools: Vec<String> = ["Read", "Grep", "Glob"].iter().map(|s| s.to_string()).collect();
    tools.extend(kb::zbrain_tools_for(Caller::SupervisorAgent));
    tools
}

/// The brief the agent starts from: the finding and the limits above.
pub fn brief(f: &Finding) -> String {
    format!(
        "Run {run} scored 0 on '{id}': {criterion}.\nEvidence quoted by the scorer: {quote}\n\n\
         Read the run, put one evidence-backed question to the Lead, then answer with the six fields: \
         observation, evidence, suspected mechanism, impact, question, recommendation. \
         You are read-only: write no code, give no order, accept nothing.",
        run = f.run_id,
        id = f.rubric.id,
        criterion = f.rubric.criterion,
        quote = f.quote,
    )
}

/// The agent in its plain form, no model call: the six fields come from the
/// finding. The change is a review of one run, reversible and inside the
/// run's project, so a run with no project still files high.
pub struct TemplateAgent;

impl AgentRunner for TemplateAgent {
    fn investigate(&self, f: &Finding) -> Result<Answer, String> {
        let a = Answer {
            text: Text {
                observation: Some(format!("run {} scored 0 on '{}'", f.run_id, f.rubric.id)),
                evidence: Some(f.quote.clone()),
                suspected_mechanism: Some(f.rubric.criterion.clone()),
                impact: Some("not measured; the Lead's answer to the question decides it".into()),
                question: Some(format!(
                    "Run {}: {}. What in the brief or the evidence explains it?",
                    f.run_id, f.rubric.criterion
                )),
                recommendation: Some(format!("review run {} against '{}'", f.run_id, f.rubric.id)),
            },
            change: Change {
                reversible: true,
                projects: 1,
                surfaces: vec![],
                external_effect: false,
            },
        };
        a.check()?;
        Ok(a)
    }
}
