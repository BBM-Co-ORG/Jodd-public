//! Checklist task rows in a note body (spec 2026-11-19-properties-tasks-views §6.1–6.2).

/// One checklist row inside a note, in document order.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TaskItem {
    /// 0-based position of this checkbox within the note, counting every
    /// checkbox regardless of state. Absolute — `set_task_state` (Task 8d)
    /// takes this index, so filtering by `include_done` must never renumber.
    pub index: usize,
    pub text: String,
    pub checked: bool,
    /// Nesting depth, `margin-left / 28` (Jodd's own convention — see
    /// `NoteEditor.svelte:443-460` and `taskify_checklists`'s
    /// `TASK_INDENT_PX`). 0 for a top-level row.
    pub level: usize,
    /// Due date read from the row text (`📅 YYYY-MM-DD` / `due: YYYY-MM-DD`),
    /// serialized as `YYYY-MM-DD`; omitted when the row has none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due: Option<chrono::NaiveDate>,
}


/// The write side of the indent convention, imported rather than
/// re-declared: `taskify_checklists` stamps `margin-left: {28 × depth}px`
/// and this crate divides it back out. Two `const … = 28` declarations in
/// two crates, each with a comment pointing at the other, is the arrangement
/// that survives review and then drifts (finding I3).
use crate::llm::markdown::{TASK_INDENT_MAX_PX, TASK_INDENT_PX};

/// Deepest level the editor can round-trip, derived from the same pair of
/// constants the write side clamps with. A body carrying a deeper
/// `margin-left` (hand-edited, or authored elsewhere) reports the deepest
/// real level instead of one the editor could never produce.
const TASK_MAX_LEVEL: usize = TASK_INDENT_MAX_PX / TASK_INDENT_PX;

fn is_checkbox_input(node: &markup5ever_rcdom::Handle) -> bool {
    match &node.data {
        markup5ever_rcdom::NodeData::Element { name, attrs, .. } if &*name.local == "input" => {
            attrs
                .borrow()
                .iter()
                .any(|a| &*a.name.local == "type" && a.value.eq_ignore_ascii_case("checkbox"))
        }
        _ => false,
    }
}

fn element_attr(node: &markup5ever_rcdom::Handle, key: &str) -> Option<String> {
    match &node.data {
        markup5ever_rcdom::NodeData::Element { attrs, .. } => attrs
            .borrow()
            .iter()
            .find(|a| &*a.name.local == key)
            .map(|a| a.value.to_string()),
        _ => None,
    }
}

/// `margin-left: <n>px` out of a `style` attribute value, Jodd's only use of
/// inline style on a task row (see `is_margin_left_only` in
/// `llm/markdown.rs`).
fn margin_left_px(style: &str) -> Option<usize> {
    for decl in style.split(';') {
        let (prop, val) = decl.split_once(':')?;
        if prop.trim().eq_ignore_ascii_case("margin-left") {
            return val.trim().strip_suffix("px")?.parse().ok();
        }
    }
    None
}

/// Concatenate every text descendant of `row`, skipping the checkbox
/// `<input>` itself (it carries no text), and trim the result — the row's
/// "text content" the brief specifies.
fn row_text(row: &markup5ever_rcdom::Handle) -> String {
    fn walk(node: &markup5ever_rcdom::Handle, out: &mut String) {
        match &node.data {
            markup5ever_rcdom::NodeData::Text { contents } => out.push_str(&contents.borrow()),
            markup5ever_rcdom::NodeData::Element { .. } => {
                for child in node.children.borrow().iter() {
                    walk(child, out);
                }
            }
            _ => {}
        }
    }
    let mut out = String::new();
    for child in row.children.borrow().iter() {
        if is_checkbox_input(child) {
            continue;
        }
        walk(child, &mut out);
    }
    out.trim().to_string()
}

/// Document-order scan for checkbox rows. `node`'s children are walked in
/// order; when a child is a checkbox `<input>`, `node` is the row (Jodd's
/// task rows are `<div><input type="checkbox">...text</div>`, so the
/// checkbox's immediate parent is always the row) and a `TaskItem` is
/// recorded immediately — *before* recursing further — so the resulting
/// order is a true preorder walk, matching the byte order the HTML source
/// was written in.
///
/// This must equal `set_checkbox_state`'s byte-order tag scan (Task 8d):
/// that function counts `<input>` tags as it meets them scanning forward
/// through the raw bytes, which is exactly source/document order. A
/// two-pass version of this walk — record every direct-child checkbox of
/// `node` first, THEN recurse into `node`'s children — produces level order
/// instead: a checkbox nested one level deeper than a later sibling
/// checkbox would be emitted *after* it, even though it appears earlier in
/// the byte stream. `expect_text`/index-based callers (`set_task_state`)
/// rely on this function and the byte scanner agreeing on what "index N"
/// means, so the single interleaved pass below — recurse immediately after
/// recording, before moving to the next sibling — is load-bearing, not a
/// style choice. See `parse_tasks_and_set_checkbox_state_agree_on_nested_checkboxes`.
fn collect_tasks(node: &markup5ever_rcdom::Handle, out: &mut Vec<TaskItem>) {
    let children = node.children.borrow();
    for child in children.iter() {
        if is_checkbox_input(child) {
            let level = element_attr(node, "style")
                .and_then(|s| margin_left_px(&s))
                .map(|px| (px / TASK_INDENT_PX).min(TASK_MAX_LEVEL))
                .unwrap_or(0);
            let text = row_text(node);
            let due = crate::props::parse_due(&text);
            out.push(TaskItem {
                index: out.len(),
                text,
                checked: element_attr(child, "checked").is_some(),
                level,
                due,
            });
        }
        collect_tasks(child, out);
    }
}

/// Parse every checkbox row out of a note body, in document order. `index`
/// is the 0-based position of each `input[type=checkbox]` regardless of
/// state — callers that filter by `checked` must not renumber afterwards
/// (see `TaskItem::index`).
pub fn parse_tasks(body_html: &str) -> Vec<TaskItem> {
    use html5ever::tendril::TendrilSink;
    use markup5ever_rcdom::RcDom;

    let dom = html5ever::parse_fragment(
        RcDom::default(),
        Default::default(),
        html5ever::QualName::new(None, html5ever::ns!(html), html5ever::local_name!("body")),
        vec![],
        false,
    )
    .one(body_html);

    let mut tasks = Vec::new();
    if let Some(root) = dom.document.children.borrow().first().cloned() {
        collect_tasks(&root, &mut tasks);
    }
    tasks
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::markdown::{TASK_INDENT_MAX_PX, TASK_INDENT_PX};

    #[test]
    fn parse_tasks_reads_state_order_and_depth() {
        let body = concat!(
            r#"<div><input type="checkbox" contenteditable="false">&nbsp;first</div>"#,
            r#"<div style="margin-left: 28px"><input type="checkbox" checked="" contenteditable="false">&nbsp;nested done</div>"#,
        );
        let tasks = parse_tasks(body);
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].index, 0);
        assert_eq!(tasks[0].text, "first");
        assert!(!tasks[0].checked);
        assert_eq!(tasks[0].level, 0);
        assert_eq!(tasks[1].index, 1);
        assert_eq!(tasks[1].text, "nested done");
        assert!(tasks[1].checked);
        assert_eq!(tasks[1].level, 1);
    }

    #[test]
    fn parse_tasks_preserves_inline_markup_in_text() {
        // Bold text and a link's anchor text must both survive into `text`.
        let body = concat!(
            r#"<div><input type="checkbox" contenteditable="false">"#,
            r#"&nbsp;Ship <strong>v2</strong> per the <a href="https://example.com/spec">spec</a></div>"#,
        );
        let tasks = parse_tasks(body);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].text, "Ship v2 per the spec", "got: {:?}", tasks[0].text);
    }

    /// `level` shares the write side's scale and ceiling.
    #[test]
    fn task_level_shares_the_write_sides_scale_and_ceiling() {
        let body = format!(
            concat!(
                r#"<div style="margin-left: {}px"><input type="checkbox">&nbsp;three</div>"#,
                r#"<div style="margin-left: {}px"><input type="checkbox">&nbsp;absurd</div>"#,
            ),
            TASK_INDENT_PX * 3,
            TASK_INDENT_MAX_PX * 4,
        );
        let tasks = parse_tasks(&body);
        assert_eq!(tasks[0].level, 3, "must use the same 28px scale the writer stamps");
        assert_eq!(tasks[1].level, TASK_INDENT_MAX_PX / TASK_INDENT_PX,
            "deeper than the editor can round-trip clamps to the deepest real level");
    }

    #[test]
    fn due_is_read_from_row_text() {
        let body = r#"<div class="jodd-task"><input type="checkbox" contenteditable="false">&nbsp;ship 📅 2026-11-25</div>"#;
        let tasks = parse_tasks(body);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].due, chrono::NaiveDate::from_ymd_opt(2026, 11, 25));
        assert!(tasks[0].text.contains("📅 2026-11-25"), "{:?}", tasks[0].text);
    }

    #[test]
    fn due_serializes_as_iso_and_is_omitted_when_none() {
        let body = concat!(
            r#"<div><input type="checkbox">&nbsp;a due: 2026-12-01</div>"#,
            r#"<div><input type="checkbox">&nbsp;b</div>"#,
        );
        let tasks = parse_tasks(body);
        let v = serde_json::to_value(&tasks).unwrap();
        assert_eq!(v[0]["due"], "2026-12-01");
        assert!(v[1].get("due").is_none(), "{v}");
        assert_eq!(v[1]["index"], 1);
        assert_eq!(v[1]["checked"], false);
        assert_eq!(v[1]["level"], 0);
    }

    #[test]
    fn indices_unchanged_on_icloud_and_ssh_shapes() {
        // What `icloud::format_html::render_paragraphs` emits for todo lines.
        let icloud = concat!(
            r#"<div>intro</div>"#,
            r#"<div class="jodd-task"><input type="checkbox" checked contenteditable="false">&nbsp;one</div>"#,
            r#"<div class="jodd-task" style="margin-left: 28px"><input type="checkbox" contenteditable="false">&nbsp;two</div>"#,
            r#"<div class="jodd-task"><input type="checkbox" contenteditable="false">&nbsp;three</div>"#,
        );
        let t = parse_tasks(icloud);
        assert_eq!(t.iter().map(|x| x.index).collect::<Vec<_>>(), vec![0, 1, 2]);
        assert_eq!(t.iter().map(|x| x.text.as_str()).collect::<Vec<_>>(), vec!["one", "two", "three"]);
        assert_eq!(t.iter().map(|x| x.checked).collect::<Vec<_>>(), vec![true, false, false]);
        assert_eq!(t[1].level, 1);

        // SSH: a Markdown file decoded by the real `ssh::file::decode`.
        let d = crate::backend::ssh::file::decode(
            "Notes/Inbox/t.md",
            b"# T\n\n- [ ] first\n- [x] second\n- [ ] third due: 2026-11-30\n",
        )
        .unwrap();
        let t = parse_tasks(&d.body_html);
        assert_eq!(t.iter().map(|x| x.index).collect::<Vec<_>>(), vec![0, 1, 2]);
        assert_eq!(t[0].text, "first");
        assert!(t[1].checked);
        assert_eq!(t[2].due, chrono::NaiveDate::from_ymd_opt(2026, 11, 30));
    }
}
