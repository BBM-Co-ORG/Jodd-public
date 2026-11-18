//! Research Studio: generate a Briefing, FAQ, Timeline or Study guide — or a
//! deliverable draft: a Slide deck, Course module, Podcast script or Video
//! script — from a folder's notes, as a new note in which every claim is cited.

use crate::llm::provider::WorkflowKind;

pub mod cite;
pub mod plan;
pub mod related;
pub mod run;

/// What a Studio run produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StudioOutput {
    Briefing,
    Faq,
    Timeline,
    StudyGuide,
    /// Deliverable drafts (spec 2026-10-26): text shaped for a later renderer.
    SlideDeck,
    CourseModule,
    PodcastScript,
    VideoScript,
}

impl StudioOutput {
    /// The workflow whose prompt writes this output.
    pub fn workflow(self) -> WorkflowKind {
        match self {
            StudioOutput::Briefing => WorkflowKind::StudioBriefing,
            StudioOutput::Faq => WorkflowKind::StudioFaq,
            StudioOutput::Timeline => WorkflowKind::StudioTimeline,
            StudioOutput::StudyGuide => WorkflowKind::StudioStudyGuide,
            StudioOutput::SlideDeck => WorkflowKind::StudioSlideDeck,
            StudioOutput::CourseModule => WorkflowKind::StudioCourseModule,
            StudioOutput::PodcastScript => WorkflowKind::StudioPodcastScript,
            StudioOutput::VideoScript => WorkflowKind::StudioVideoScript,
        }
    }

    /// The word that opens the generated note's title.
    pub fn title_prefix(self) -> &'static str {
        match self {
            StudioOutput::Briefing => "Briefing",
            StudioOutput::Faq => "FAQ",
            StudioOutput::Timeline => "Timeline",
            StudioOutput::StudyGuide => "Study guide",
            StudioOutput::SlideDeck => "Slide deck",
            StudioOutput::CourseModule => "Course module",
            StudioOutput::PodcastScript => "Podcast script",
            StudioOutput::VideoScript => "Video script",
        }
    }

    /// The four drafts to deliver (spec 2026-10-26), whose marker lines
    /// `run::separate_marker_lines` keeps in paragraphs of their own.
    pub fn is_deliverable(self) -> bool {
        match self {
            StudioOutput::SlideDeck | StudioOutput::CourseModule | StudioOutput::PodcastScript | StudioOutput::VideoScript => true,
            StudioOutput::Briefing | StudioOutput::Faq | StudioOutput::Timeline | StudioOutput::StudyGuide => false,
        }
    }

    /// The English name, used inside AI payloads (`Output: {name}`).
    pub fn name(self) -> &'static str {
        self.title_prefix()
    }
}

/// The footer line that marks a note as made by Studio.
pub const STUDIO_MARKER: &str = "Made with Jodd Studio";

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [(StudioOutput, &str); 8] = [
        (StudioOutput::Briefing, "briefing"),
        (StudioOutput::Faq, "faq"),
        (StudioOutput::Timeline, "timeline"),
        (StudioOutput::StudyGuide, "study_guide"),
        (StudioOutput::SlideDeck, "slide_deck"),
        (StudioOutput::CourseModule, "course_module"),
        (StudioOutput::PodcastScript, "podcast_script"),
        (StudioOutput::VideoScript, "video_script"),
    ];

    /// `src/lib/studio.ts` sends these exact strings; there is no rename layer.
    #[test]
    fn every_output_round_trips_by_its_serde_name() {
        for (output, name) in ALL {
            assert_eq!(serde_json::to_string(&output).unwrap(), format!("\"{name}\""));
            assert_eq!(serde_json::from_str::<StudioOutput>(&format!("\"{name}\"")).unwrap(), output);
        }
    }

    #[test]
    fn deliverables_use_their_own_workflow_and_prefix() {
        let want = [
            (StudioOutput::SlideDeck, WorkflowKind::StudioSlideDeck, "Slide deck"),
            (StudioOutput::CourseModule, WorkflowKind::StudioCourseModule, "Course module"),
            (StudioOutput::PodcastScript, WorkflowKind::StudioPodcastScript, "Podcast script"),
            (StudioOutput::VideoScript, WorkflowKind::StudioVideoScript, "Video script"),
        ];
        for (output, kind, prefix) in want {
            assert_eq!(output.workflow(), kind);
            assert!(kind.is_studio(), "{}", kind.slug());
            assert_eq!(output.title_prefix(), prefix);
            assert_eq!(output.name(), prefix);
        }
    }
}
