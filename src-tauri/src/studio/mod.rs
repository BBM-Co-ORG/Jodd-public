//! Research Studio: generate a Briefing, FAQ, Timeline or Study guide from a
//! folder's notes, as a new note in which every claim is cited.

use crate::llm::provider::WorkflowKind;

pub mod cite;
pub mod plan;
pub mod run;

/// What a Studio run produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StudioOutput {
    Briefing,
    Faq,
    Timeline,
    StudyGuide,
}

impl StudioOutput {
    /// The workflow whose prompt writes this output.
    pub fn workflow(self) -> WorkflowKind {
        match self {
            StudioOutput::Briefing => WorkflowKind::StudioBriefing,
            StudioOutput::Faq => WorkflowKind::StudioFaq,
            StudioOutput::Timeline => WorkflowKind::StudioTimeline,
            StudioOutput::StudyGuide => WorkflowKind::StudioStudyGuide,
        }
    }

    /// The word that opens the generated note's title.
    pub fn title_prefix(self) -> &'static str {
        match self {
            StudioOutput::Briefing => "Briefing",
            StudioOutput::Faq => "FAQ",
            StudioOutput::Timeline => "Timeline",
            StudioOutput::StudyGuide => "Study guide",
        }
    }

    /// The English name, used inside AI payloads (`Output: {name}`).
    pub fn name(self) -> &'static str {
        self.title_prefix()
    }
}

/// The footer line that marks a note as made by Studio.
pub const STUDIO_MARKER: &str = "Made with Jodd Studio";
