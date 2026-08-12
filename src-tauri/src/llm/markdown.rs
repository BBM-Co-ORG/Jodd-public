//! Markdown → HTML conversion + Lessons note body assembly.

use std::cell::RefCell;
use std::rc::Rc;

use markup5ever_rcdom::{Handle, Node, NodeData};
use pulldown_cmark::{html, Options, Parser};

use crate::llm::provider::ExtractEnvelope;

/// Marker for the collapsible Source block. Single source of truth for
/// `assemble_note_body` (writer), `has_source_block` (detector), and
/// `extract_source` (parser) — change here and all three sites stay in sync.
const SOURCE_MARKER: &str = "<summary>Source (verbatim)</summary>";

/// GFM extensions LLMs commonly emit. Tables turn `| col1 | col2 |\n|---|---|`
/// into a real <table>; strikethrough handles `~~text~~`; tasklists render
/// `- [x] done` as `<input type="checkbox">`; footnotes pair `[^1]` with
/// their definitions. Without these, pulldown-cmark's defaults leave the
/// raw markdown syntax visible in the rendered HTML.
fn md_options() -> Options {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    opts.insert(Options::ENABLE_FOOTNOTES);
    opts
}

/// Convert a markdown string to HTML. Pure function, no escaping issues —
/// pulldown-cmark handles all the markdown-specific encoding.
pub fn md_to_html(md: &str) -> String {
    let parser = Parser::new_ext(md, md_options());
    let mut out = String::new();
    html::push_html(&mut out, parser);
    out
}

/// HTML-escape arbitrary text for safe inclusion in HTML.
pub fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Build the tags-line + lessons + optional-meta + source-block fragment for
/// one ingest. Shared by `assemble_note_body` (fresh note) and
/// `append_to_note_body` (existing note) so both produce byte-identical
/// fragments — the only difference is what (if anything) comes before it.
fn build_ingest_fragment(envelope: &ExtractEnvelope, source: &str) -> String {
    let mut body = String::with_capacity(envelope.lessons_markdown.len() + source.len() + 512);

    // Tags line at the top, picked up by Jodd's existing #hashtag parser
    if !envelope.tags.is_empty() {
        body.push_str("<p>");
        for (i, tag) in envelope.tags.iter().enumerate() {
            if i > 0 {
                body.push(' ');
            }
            body.push('#');
            // LLMs sometimes emit `#tag` or multi-word tags; normalize so
            // Jodd's #hashtag parser picks them up (strip leading #,
            // collapse whitespace to '-' so the tag is a single token).
            let clean = tag.trim_start_matches('#').replace(char::is_whitespace, "-");
            body.push_str(&escape_html(&clean));
        }
        body.push_str("</p>\n");
    }

    // Main lessons content
    body.push_str(&md_to_html(&envelope.lessons_markdown));

    // Optional meta-lessons section
    if let Some(meta) = &envelope.meta_lessons_markdown {
        if !meta.trim().is_empty() {
            body.push_str(&md_to_html(meta));
        }
    }

    // Collapsible source section — pure HTML, source verbatim in <pre>
    body.push_str("<hr>\n<details>\n");
    body.push_str(SOURCE_MARKER);
    body.push_str("\n<pre>");
    body.push_str(&escape_html(source));
    body.push_str("</pre>\n</details>\n");

    body
}

/// Assemble the final note body from an envelope + raw source text.
pub fn assemble_note_body(envelope: &ExtractEnvelope, source: &str) -> String {
    build_ingest_fragment(envelope, source)
}

/// Append a new ingest's content to an existing note body. Pure concatenation
/// — never edits or reorders what's already there. The tags line, lessons
/// markdown, optional meta section, and a fresh `<details>` source block are
/// built exactly like `assemble_note_body` and appended after `existing_body`.
/// A note appended into multiple times accumulates multiple
/// `<details>Source</details>` blocks, one per ingest, in chronological order.
pub fn append_to_note_body(existing_body: &str, envelope: &ExtractEnvelope, source: &str) -> String {
    format!("{existing_body}{}", build_ingest_fragment(envelope, source))
}

/// Regex match for whether a note body contains a preserved Source block.
/// Extract the raw source text from a note body that has a Source block.
/// Returns None if no block found or the structure is malformed.
pub fn extract_source(body_html: &str) -> Option<String> {
    let after_marker = body_html.split_once(SOURCE_MARKER)?.1;
    let pre_open = after_marker.find("<pre>")?;
    let after_pre = &after_marker[pre_open + "<pre>".len()..];
    let pre_close = after_pre.find("</pre>")?;
    let raw = &after_pre[..pre_close];
    // Unescape the four entities we inject
    Some(
        raw.replace("&quot;", "\"")
            .replace("&gt;", ">")
            .replace("&lt;", "<")
            .replace("&amp;", "&"),
    )
}

/// The STRICT allowlist: what must never be silently destroyed. The fidelity
/// manifest's SHARED tier plus everything md_to_html (GFM) and Extract's
/// hand-built fragments emit — **minus `<input>`**, which lives in the
/// permissive list only.
///
/// This is the list `is_replace_safe` measures against, so widening it weakens
/// the replace-guard. Anything an agent needs in order to *write* but that must
/// still block a destructive full-body replace belongs in `note_html_builder`,
/// which extends this — never here.
fn strict_note_html_builder() -> ammonia::Builder<'static> {
    let mut b = ammonia::Builder::default();
    b.tags(
        [
            "h1", "h2", "h3", "h4", "h5", "h6", "p", "div", "span", "br", "hr", "b", "strong", "i",
            "em", "u", "del", "blockquote", "code", "pre", "ul", "ol", "li", "a", "table", "thead",
            "tbody", "tr", "th", "td", "details", "summary", "sup",
        ]
        .into_iter()
        .collect(),
    )
    .link_rel(None) // default injects rel="noopener noreferrer" → false canon mismatch on every <a>
    // Forced by md_to_html's footnote rendering, which emits
    // `<sup class="footnote-reference">`, `<div class="footnote-definition" id="1">`
    // and `<sup class="footnote-definition-label">`. Both attributes are inert —
    // no script, no navigation — so allowing them generically costs nothing.
    .add_generic_attributes(["class", "id"])
    .add_tag_attributes("a", ["href"])
    .add_tag_attributes("th", ["style", "align"])
    .add_tag_attributes("td", ["style", "align"]);
    b
}

/// The PERMISSIVE allowlist: what an agent may write. The strict list plus
/// `<input>`, which GFM tasklists (`- [x] done`) force — `md_to_html` emits
/// `<input type="checkbox" disabled>` and the
/// `pure_markdown_output_survives_sanitize` test enforces that it survives.
///
/// `contenteditable` on `<input>` and `style` on `<div>` are what
/// `taskify_checklists` stamps onto a Jodd task row; without them the
/// conversion would be undone one step later, here.
///
/// Derived from `strict_note_html_builder` on purpose: the two lists differ at
/// exactly one place, so a tag added here for the write path cannot silently
/// widen the replace-guard.
fn note_html_builder() -> ammonia::Builder<'static> {
    let mut b = strict_note_html_builder();
    b.add_tags(["input"])
        .add_tag_attributes("input", ["type", "checked", "disabled", "contenteditable"])
        .add_tag_attributes("div", ["style"])
        // `style` on a div exists for ONE reason: the checklist indent
        // `taskify_checklists` stamps. Ammonia's generic allowlist is
        // per-attribute, not per-value, so without this filter allowing
        // `style` at all would let arbitrary CSS ride in on an agent-written
        // div. `th`/`td` keep their unfiltered `style` (markdown table
        // alignment) — hence the match on the element name, not a global
        // `filter_style_properties`.
        .attribute_filter(|element, attribute, value| match (element, attribute) {
            ("div", "style") if !is_margin_left_only(value) => None,
            _ => Some(value.into()),
        });
    b
}

/// True when a `style` value declares nothing but `margin-left: <n>px`.
/// Deliberately literal — it has to admit exactly what `flatten_task_list`
/// stamps on a row and nothing an agent might smuggle past it.
fn is_margin_left_only(style: &str) -> bool {
    let mut saw_one = false;
    for decl in style.split(';') {
        if decl.trim().is_empty() {
            continue;
        }
        let Some((prop, val)) = decl.split_once(':') else {
            return false;
        };
        if !prop.trim().eq_ignore_ascii_case("margin-left") {
            return false;
        }
        let px = val.trim();
        let Some(digits) = px.strip_suffix("px") else {
            return false;
        };
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
        saw_one = true;
    }
    saw_one
}

/// Filter agent-supplied HTML down to the permissive subset. Applied ONLY to
/// new agent-authored fragments on the way in — never to an existing note body,
/// which may carry Apple markup this drops (an
/// `<object type="application/x-apple-msg-attachment">` attachment reference
/// above all).
///
/// Note the deliberate asymmetry with `is_replace_safe`: a checklist
/// `<input checked>` passes *here*, because tasklists have to be writable, yet
/// makes a body replace-*unsafe*. "What may an agent write?" and "what must
/// never be silently destroyed?" are different questions, hence two lists.
pub fn sanitize_note_html(html: &str) -> String {
    note_html_builder().clean(html).to_string()
}

/// Parse an HTML fragment and run `f` over the synthetic `<html>` node
/// `parse_fragment` roots the content under. `None` for input that produces no
/// tree at all.
///
/// The closure shape is deliberate, not ceremony. The `RcDom` has to outlive
/// every `Handle` taken from it — `Drop for Node` recursively *empties* the
/// tree, so a root handed back past its dom comes back childless and every
/// caller silently sees an empty document. Handing the root to a closure while
/// `dom` is still a live local makes that the compiler's problem rather than a
/// comment's.
fn with_fragment_root<R>(html: &str, f: impl FnOnce(&Handle) -> R) -> Option<R> {
    use html5ever::tendril::TendrilSink;
    use markup5ever_rcdom::RcDom;

    let dom = html5ever::parse_fragment(
        RcDom::default(),
        Default::default(),
        html5ever::QualName::new(None, html5ever::ns!(html), html5ever::local_name!("body")),
        vec![],
        false,
    )
    .one(html);
    let root = dom.document.children.borrow().first().cloned()?;
    Some(f(&root))
}

/// Serialize a node's children through the same html5ever serializer ammonia
/// uses, so output from here and from `Builder::clean` are byte-comparable.
fn serialize_children(root: &Handle) -> String {
    use html5ever::serialize::{serialize, SerializeOpts, TraversalScope};
    use markup5ever_rcdom::SerializableHandle;

    let mut out = Vec::new();
    let _ = serialize(
        &mut out,
        &SerializableHandle::from(root.clone()),
        SerializeOpts {
            traversal_scope: TraversalScope::ChildrenOnly(None),
            ..Default::default()
        },
    );
    String::from_utf8(out).unwrap_or_default()
}

/// Parse + re-serialize with NO filtering, through the same html5ever
/// serializer ammonia uses. Two purposes: (1) cancel serializer cosmetics
/// so `is_replace_safe` measures actual stripping, not quoting style;
/// (2) never used on the write path — existing bodies are stored untouched.
pub fn canonicalize_note_html(html: &str) -> String {
    with_fragment_root(html, serialize_children).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// GFM tasklists → Jodd task rows
// ---------------------------------------------------------------------------

/// One indent level, in pixels. Mirrors `NoteEditor.svelte`, where
/// `propagateChecklist` derives a row's nesting level as `marginLeft / 28`.
///
/// `pub` because the read side of the same convention lives in another crate:
/// jodd-mcp's `parse_tasks` divides `margin-left` back into a `level` for
/// `list_tasks`. It used to re-declare `28` with a comment pointing here —
/// the arrangement that survives review and then drifts (finding I3).
pub const TASK_INDENT_PX: usize = 28;
/// Deepest indent the editor will produce (`NoteEditor.svelte:504` clamps to
/// 168px = 6 levels). Deeper markdown nesting flattens onto the last level
/// rather than inventing an indent the editor can never round-trip. The read
/// side clamps to the same ceiling, so a hand-authored 500px margin reports
/// the deepest level the editor can actually round-trip rather than an
/// invented one.
pub const TASK_INDENT_MAX_PX: usize = 168;

/// Rewrite GFM tasklists into Jodd's native task rows.
///
/// GFM compiles `- [ ] x` to `<ul><li><input disabled type="checkbox">x</li></ul>`,
/// which is a *decoration*: `disabled` blocks ticking outright, and
/// `taskBlock()` (`NoteEditor.svelte:433`) only recognizes a row whose checkbox
/// is a direct child of a top-level block, so Enter and indent never apply.
/// A checkbox in a Jodd note is user state, not formatting, so an agent must be
/// able to write one somebody can actually tick — hence this step, which sits
/// between `md_to_html` and `sanitize_note_html` on the write path.
///
/// **Scope rule: a list converts only if EVERY one of its `<li>` children is a
/// task item.** A mixed list is left alone — today's behavior, deliberately,
/// rather than splitting a list the author wrote as one thing.
///
/// Two different strengths of "unchanged", worth not confusing:
/// - If **nothing at all** converts, the input `&str` is returned verbatim. No
///   parse-and-reserialize, so this is a true no-op rather than a
///   canonicalization for every body without a tasklist.
/// - If **anything** converts, the whole fragment is re-serialized, so lists
///   left alone elsewhere in the same document still pick up serializer
///   cosmetics (`<input …/>` → `<input …>`). Harmless on the write path —
///   `sanitize_note_html` runs next and produces exactly those anyway — but it
///   is not byte-identity.
pub fn taskify_checklists(html: &str) -> String {
    with_fragment_root(html, |root| {
        rewrite_task_lists(root).then(|| serialize_children(root))
    })
    .flatten()
    .unwrap_or_else(|| html.to_string())
}

fn is_element(node: &Handle, tag: &str) -> bool {
    matches!(&node.data, NodeData::Element { name, .. } if &*name.local == tag)
}

fn attr_value(node: &Handle, key: &str) -> Option<String> {
    match &node.data {
        NodeData::Element { attrs, .. } => attrs
            .borrow()
            .iter()
            .find(|a| &*a.name.local == key)
            .map(|a| a.value.to_string()),
        _ => None,
    }
}

fn is_list(node: &Handle) -> bool {
    is_element(node, "ul") || is_element(node, "ol")
}

/// The `<li>`'s leading checkbox, if it has one. "Leading" means the first
/// *element* child — the shape GFM emits for a tight list. A loose list wraps
/// the item in `<p>` and therefore does not qualify, which is the conservative
/// answer: leave it as parsed.
fn task_checkbox(li: &Handle) -> Option<Handle> {
    let first = li
        .children
        .borrow()
        .iter()
        .find(|c| matches!(c.data, NodeData::Element { .. }))
        .cloned()?;
    let is_checkbox = is_element(&first, "input")
        && attr_value(&first, "type").is_some_and(|t| t.eq_ignore_ascii_case("checkbox"));
    is_checkbox.then_some(first)
}

fn is_task_list(list: &Handle) -> bool {
    let children = list.children.borrow();
    let mut items = children.iter().filter(|c| is_element(c, "li")).peekable();
    items.peek().is_some() && items.all(|li| task_checkbox(li).is_some())
}

fn new_element(tag: &str, attrs: Vec<html5ever::Attribute>) -> Handle {
    Node::new(NodeData::Element {
        name: html5ever::QualName::new(None, html5ever::ns!(html), tag.into()),
        attrs: RefCell::new(attrs),
        template_contents: RefCell::new(None),
        mathml_annotation_xml_integration_point: false,
    })
}

fn new_attr(name: &str, value: &str) -> html5ever::Attribute {
    html5ever::Attribute {
        name: html5ever::QualName::new(None, html5ever::ns!(), name.into()),
        value: value.into(),
    }
}

/// Append `child` to `parent`, re-pointing its parent link. Unlike rcdom's own
/// (private) `append`, this accepts a node that already had a parent — every
/// move here is out of an `<li>` that is being dismantled. The caller must have
/// *detached* it from that `<li>` first; see `detach_children`.
fn adopt(parent: &Handle, child: Handle) {
    child.parent.set(Some(Rc::downgrade(parent)));
    parent.children.borrow_mut().push(child);
}

/// Take a node's children OUT of it, transferring ownership to the caller.
///
/// This is not a convenience — it is the only thing that makes moving a node
/// between trees safe. `Drop for Node` (markup5ever_rcdom) does not just free a
/// node: it walks the whole subtree and `mem::take`s the children of *every*
/// node it reaches. So a node still listed in a doomed parent's `children` gets
/// **emptied** when that parent drops, even though the node itself is still
/// alive and reachable from somewhere else.
///
/// Cloning a `Handle` out of a `<li>` and leaving the original entry behind
/// therefore hollows out the clone the moment the `<li>` goes. Text survives
/// (its content lives in `NodeData::Text`, not in `children`), which makes the
/// bug invisible to any test whose task items are bare text — the first version
/// of this code shipped `Ship <strong></strong> by Friday`.
fn detach_children(node: &Handle) -> Vec<Handle> {
    let kids = std::mem::take(&mut *node.children.borrow_mut());
    for kid in &kids {
        kid.parent.set(None);
    }
    kids
}

/// Walk `parent`'s children, replacing every all-task-item list with the rows
/// it flattens to. Returns true if anything was replaced.
fn rewrite_task_lists(parent: &Handle) -> bool {
    let old = detach_children(parent);
    let mut out: Vec<Handle> = Vec::with_capacity(old.len());
    let mut changed = false;
    for child in old {
        if is_list(&child) && is_task_list(&child) {
            flatten_task_list(&child, 0, &mut out);
            changed = true;
        } else {
            changed |= rewrite_task_lists(&child);
            out.push(child);
        }
    }
    for child in &out {
        child.parent.set(Some(Rc::downgrade(parent)));
    }
    *parent.children.borrow_mut() = out;
    changed
}

/// Emit one `<div>` row per `<li>`, appending them to `out`. A nested task list
/// inside an item does not stay nested — it flattens into the following
/// siblings at `depth + 1`, because Jodd expresses nesting as the row's
/// margin, not as containment.
fn flatten_task_list(
    list: &Handle,
    depth: usize,
    out: &mut Vec<Handle>,
) {
    let children = list.children.borrow();
    for li in children.iter().filter(|c| is_element(c, "li")) {
        let Some(checkbox) = task_checkbox(li) else {
            continue; // unreachable: is_task_list() gated this
        };
        // MUST happen before the `<li>` is dropped, and before we hand any of
        // these nodes to `adopt`. See `detach_children` for why.
        let kids = detach_children(li);

        let mut row_attrs = Vec::new();
        if depth > 0 {
            let px = (depth * TASK_INDENT_PX).min(TASK_INDENT_MAX_PX);
            row_attrs.push(new_attr("style", &format!("margin-left: {px}px")));
        }
        let row = new_element("div", row_attrs);

        // `disabled` is dropped (it is exactly what makes a GFM checkbox
        // untickable) and `contenteditable="false"` added, which is what keeps
        // a click on the box toggling it instead of placing a caret.
        let mut input_attrs = vec![
            new_attr("type", "checkbox"),
            new_attr("contenteditable", "false"),
        ];
        if attr_value(&checkbox, "checked").is_some() {
            input_attrs.push(new_attr("checked", ""));
        }
        adopt(&row, new_element("input", input_attrs));
        adopt(
            &row,
            Node::new(NodeData::Text {
                contents: RefCell::new("\u{a0}".into()),
            }),
        );

        let mut content = Vec::new();
        let mut nested = Vec::new();
        for kid in kids {
            if Rc::ptr_eq(&kid, &checkbox) {
                continue;
            }
            if is_list(&kid) && is_task_list(&kid) {
                nested.push(kid);
            } else {
                rewrite_task_lists(&kid);
                content.push(kid);
            }
        }
        trim_edge_whitespace(&mut content);
        for kid in content {
            adopt(&row, kid);
        }
        out.push(row);

        for sub in nested {
            flatten_task_list(&sub, depth + 1, out);
        }
    }
}

/// Strip the line breaks markdown leaves around an item's text, so a row
/// serializes as a single clean line rather than `&nbsp;\nparent\n`.
///
/// Two of them, at different places: one right after the `<input>`, and — when
/// the item had a nested list — one after the `</ul>`, i.e. in a *separate*
/// trailing text node. Dropping the blank edge nodes first is what makes the
/// second case reach the node that actually holds the text.
fn trim_edge_whitespace(content: &mut Vec<Handle>) {
    let text_of = |node: &Handle| match &node.data {
        NodeData::Text { contents } => Some(contents.borrow().to_string()),
        _ => None,
    };
    let set_text = |node: &Handle, value: &str| {
        if let NodeData::Text { contents } = &node.data {
            *contents.borrow_mut() = value.into();
        }
    };
    let is_blank = |node: &Handle| text_of(node).is_some_and(|t| t.trim().is_empty());

    while content.first().is_some_and(is_blank) {
        content.remove(0);
    }
    while content.last().is_some_and(is_blank) {
        content.pop();
    }
    if let Some(first) = content.first() {
        if let Some(text) = text_of(first) {
            set_text(first, text.trim_start());
        }
    }
    if let Some(last) = content.last() {
        if let Some(text) = text_of(last) {
            set_text(last, text.trim_end());
        }
    }
}

/// The replace-guard: true when a destructive full-body replace would destroy
/// nothing an agent could not have written itself.
///
/// Measured against the STRICT list, which omits `<input>` entirely: any
/// `<input>` element makes a body replace-unsafe, checkboxes being the case
/// that motivates it — Apple's, Jodd's own editor's, or an agent's inert
/// `disabled` one alike. That is deliberately conservative: `checked` state is a
/// PRESERVED-tier fact in the fidelity manifest, and nothing in an agent's
/// Markdown can carry it back.
pub fn is_replace_safe(html: &str) -> bool {
    canonicalize_note_html(html) == strict_note_html_builder().clean(html).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md_to_html_handles_basic_markdown() {
        let html = md_to_html("## H2\n\nparagraph **bold**");
        assert!(html.contains("<h2>H2</h2>"));
        assert!(html.contains("<strong>bold</strong>"));
    }

    #[test]
    fn md_to_html_renders_gfm_tables() {
        let md = "| Col1 | Col2 |\n|---|---|\n| a | b |";
        let html = md_to_html(md);
        assert!(html.contains("<table>"), "missing <table>: {html}");
        assert!(html.contains("<th>Col1</th>"));
        assert!(html.contains("<td>a</td>"));
    }

    #[test]
    fn md_to_html_renders_strikethrough_and_tasklists() {
        assert!(md_to_html("~~struck~~").contains("<del>struck</del>"));
        let tasklist = md_to_html("- [x] done\n- [ ] todo");
        assert!(tasklist.contains("<input"), "missing tasklist input: {tasklist}");
        assert!(tasklist.contains("disabled"), "tasklist should be disabled: {tasklist}");
    }

    #[test]
    fn escape_html_escapes_all_four() {
        assert_eq!(
            escape_html("a&b<c>d\"e"),
            "a&amp;b&lt;c&gt;d&quot;e"
        );
    }

    #[test]
    fn assemble_includes_all_sections() {
        let env = ExtractEnvelope {
            title: Some("T".into()),
            lessons_markdown: "## Lesson 1\nbody".into(),
            meta_lessons_markdown: Some("## Meta\nm".into()),
            tags: vec!["tag-a".into(), "tag-b".into()],
            confidence: Some("high".into()),
        };
        let body = assemble_note_body(&env, "raw source text");
        assert!(body.contains("#tag-a #tag-b"), "tag line: {body}");
        assert!(body.contains("<h2>Lesson 1</h2>"));
        assert!(body.contains("<h2>Meta</h2>"));
        assert!(body.contains("<summary>Source (verbatim)</summary>"));
        assert!(body.contains("raw source text"));
    }

    #[test]
    fn assemble_omits_meta_when_absent_or_empty() {
        let env = ExtractEnvelope {
            title: None,
            lessons_markdown: "x".into(),
            meta_lessons_markdown: Some("   ".into()),
            tags: vec![],
            confidence: None,
        };
        let body = assemble_note_body(&env, "src");
        assert!(!body.contains("Meta"));
    }

    #[test]
    fn extract_source_roundtrips_special_chars() {
        let env = ExtractEnvelope {
            title: None,
            lessons_markdown: "x".into(),
            meta_lessons_markdown: None,
            tags: vec![],
            confidence: None,
        };
        let original = "code: <script>alert(\"hi & bye\")</script>";
        let body = assemble_note_body(&env, original);
        let recovered = extract_source(&body).unwrap();
        assert_eq!(recovered, original);
    }

    #[test]
    fn extract_source_returns_none_for_normal_note() {
        assert_eq!(extract_source("<p>just a note</p>"), None);
    }

    #[test]
    fn append_to_note_body_preserves_existing_content_as_prefix() {
        let existing = "<p>Original paragraph.</p>\n<h2>Original heading</h2>\n";
        let env = ExtractEnvelope {
            title: None,
            lessons_markdown: "## New point\nnew body".into(),
            meta_lessons_markdown: None,
            tags: vec![],
            confidence: None,
        };
        let result = append_to_note_body(existing, &env, "new source text");
        assert!(
            result.starts_with(existing),
            "existing content must be an untouched prefix: {result}"
        );
    }

    #[test]
    fn append_to_note_body_appends_new_fragment_after_existing() {
        let existing = "<p>Original.</p>\n";
        let env = ExtractEnvelope {
            title: None,
            lessons_markdown: "## New point\nbody text".into(),
            meta_lessons_markdown: Some("## Meta\nmeta text".into()),
            tags: vec!["tag-x".into()],
            confidence: None,
        };
        let result = append_to_note_body(existing, &env, "raw new source");
        assert!(result.contains("#tag-x"), "tag line: {result}");
        assert!(result.contains("<h2>New point</h2>"), "lessons heading: {result}");
        assert!(result.contains("<h2>Meta</h2>"), "meta heading: {result}");
        assert!(result.contains("raw new source"), "source block: {result}");
        assert!(result.contains(SOURCE_MARKER), "source marker: {result}");
    }

    #[test]
    fn append_to_note_body_twice_yields_two_source_blocks_in_order() {
        let env1 = ExtractEnvelope {
            title: None,
            lessons_markdown: "## First".into(),
            meta_lessons_markdown: None,
            tags: vec![],
            confidence: None,
        };
        let env2 = ExtractEnvelope {
            title: None,
            lessons_markdown: "## Second".into(),
            meta_lessons_markdown: None,
            tags: vec![],
            confidence: None,
        };
        let after_first = append_to_note_body("", &env1, "source one");
        let after_second = append_to_note_body(&after_first, &env2, "source two");

        let first_source_pos = after_second.find("source one").expect("source one present");
        let second_source_pos = after_second.find("source two").expect("source two present");
        assert!(first_source_pos < second_source_pos, "sources must stay in ingest order");
        assert_eq!(
            after_second.matches(SOURCE_MARKER).count(),
            2,
            "expected two source blocks: {after_second}"
        );
        assert!(after_second.contains("<h2>First</h2>"));
        assert!(after_second.contains("<h2>Second</h2>"));
    }

    #[test]
    fn append_to_note_body_with_empty_existing_matches_assemble_note_body() {
        let env = ExtractEnvelope {
            title: Some("T".into()),
            lessons_markdown: "## Lesson 1\nbody".into(),
            meta_lessons_markdown: None,
            tags: vec!["tag-a".into()],
            confidence: Some("high".into()),
        };
        let appended = append_to_note_body("", &env, "raw source text");
        let assembled = assemble_note_body(&env, "raw source text");
        assert_eq!(appended, assembled, "empty-prefix append must equal a fresh assemble");
    }

    #[test]
    fn sanitize_balances_malformed_passthrough() {
        // CommonMark passes raw HTML through unvalidated; sanitize must repair it.
        let dirty = md_to_html("intro <div><b>unclosed\n\nnext para");
        let clean = sanitize_note_html(&dirty);
        assert_eq!(clean.matches("<div").count(), clean.matches("</div").count());
        assert_eq!(clean.matches("<b").count(), clean.matches("</b").count());
    }

    #[test]
    fn sanitize_strips_disallowed_tags() {
        let clean = sanitize_note_html("<p>ok</p><script>alert(1)</script><object data=\"cid:x\"></object>");
        assert!(!clean.contains("<script"));
        assert!(!clean.contains("<object"));
        assert!(clean.contains("<p>ok</p>"));
    }

    #[test]
    fn pure_markdown_output_survives_sanitize() {
        // The fixed-point property the WRITE path depends on: everything
        // md_to_html can emit (GFM tables, strikethrough, tasklists,
        // footnotes) must survive sanitize unchanged after canonicalization.
        // This is the permissive list only — the replace-guard is stricter.
        let md = "# H1\n\n**bold** *it* ~~gone~~ [x](https://e.com)\n\n- [ ] task\n- [x] done\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\ntext[^1]\n\n[^1]: note\n";
        let html = md_to_html(md);
        assert_eq!(
            canonicalize_note_html(&html),
            sanitize_note_html(&html),
            "md_to_html output must round-trip sanitize"
        );
    }

    #[test]
    fn editor_style_markup_survives_both_lists_despite_cosmetics() {
        // Semantically safe but not ammonia-serialized: single quotes,
        // uppercase tag. Canonicalization must cancel those cosmetics for
        // both lists (the fixture contains no <input>, so strict and
        // permissive agree).
        let html = "<P class='x'>hi</P><div><br></div>";
        assert_eq!(canonicalize_note_html(html), sanitize_note_html(html));
        assert!(is_replace_safe(html));
    }

    #[test]
    fn checklist_markup_is_not_replace_safe() {
        // Single discriminator: a checkbox and nothing else. `checked` state
        // is PRESERVED-tier and unrepresentable in an agent's Markdown.
        assert!(!is_replace_safe(
            "<ul><li><input type=\"checkbox\" checked=\"\">done</li></ul>"
        ));
    }

    #[test]
    fn apple_attachment_object_is_not_replace_safe() {
        // Single discriminator: an Apple attachment reference and nothing else.
        assert!(!is_replace_safe(
            "<object type=\"application/x-apple-msg-attachment\" data=\"cid:abc\"></object>"
        ));
    }

    #[test]
    fn gfm_tasklist_becomes_jodd_task_rows() {
        let html = taskify_checklists(&md_to_html("- [ ] open\n- [x] done\n"));
        // Jodd's canonical row: a div, checkbox as first child, contenteditable=false, nbsp separator.
        assert!(html.contains(r#"<div><input type="checkbox" contenteditable="false">&nbsp;open</div>"#),
            "got: {html}");
        assert!(html.contains(r#"checked="""#), "checked state must survive: {html}");
        assert!(!html.contains("disabled"), "disabled blocks ticking — must be dropped: {html}");
        assert!(!html.contains("<li>"), "task items must not stay list items: {html}");
    }

    #[test]
    fn nested_tasklist_maps_depth_to_margin() {
        let html = taskify_checklists(&md_to_html("- [ ] parent\n  - [ ] child\n"));
        assert!(html.contains("margin-left: 28px") || html.contains("margin-left:28px"),
            "one level of nesting = 28px: {html}");
    }

    #[test]
    fn mixed_list_is_left_untouched() {
        // Scope rule: convert only when EVERY item is a task item.
        let src = md_to_html("- [ ] a task\n- a plain bullet\n");
        assert_eq!(taskify_checklists(&src), src);
    }

    #[test]
    fn plain_list_is_left_untouched() {
        let src = md_to_html("- one\n- two\n");
        assert_eq!(taskify_checklists(&src), src);
    }

    #[test]
    fn task_item_inline_markup_survives_the_full_pipeline() {
        // A task item is not always a bare text run. Moving an item's ELEMENT
        // children into the row without detaching them from the `<li>` first
        // lets rcdom's `Drop for Node` reach them through the discarded `<li>`
        // and `mem::take` their children — `Ship <strong></strong> by Friday`,
        // silent data loss all the way to SQLite and then Gmail.
        let out = sanitize_note_html(&taskify_checklists(&md_to_html(
            "- [ ] Ship **v2** by Friday\n",
        )));
        assert!(out.contains("<strong>v2</strong>"), "inline markup must not be emptied: {out}");
    }

    #[test]
    fn task_item_link_survives_the_full_pipeline() {
        let out = sanitize_note_html(&taskify_checklists(&md_to_html(
            "- [ ] read [docs](https://x.example/)\n",
        )));
        assert!(
            out.contains(r#"<a href="https://x.example/">docs</a>"#),
            "a link must keep its text: {out}"
        );
    }

    #[test]
    fn nested_plain_list_keeps_its_items_inside_the_row() {
        // A nested list that is NOT all-task-items stays inside the row as
        // ordinary content — with its items, not as an empty `<ul></ul>`.
        let html = taskify_checklists(&md_to_html("- [ ] parent\n  - plain sub\n"));
        assert!(html.contains("<li>plain sub</li>"), "got: {html}");
    }

    #[test]
    fn canonicalize_outlives_the_dom_it_parsed() {
        // rcdom's `Drop for Node` recursively EMPTIES the tree, so a root
        // handle that outlives its `RcDom` comes back childless. If that ever
        // regresses, canonicalize returns "" for every input and
        // `is_replace_safe` silently starts saying yes to everything.
        assert_eq!(canonicalize_note_html("<p>x</p>"), "<p>x</p>");
        assert!(!is_replace_safe("<object data=\"cid:a\"></object>"));
    }

    #[test]
    fn a_row_with_children_is_still_one_clean_line() {
        // Markdown leaves TWO line breaks around a parent item's text: one
        // after the checkbox, one after the nested `</ul>` — and the second
        // lives in its own text node, so trimming only the item's text leaves
        // `&nbsp;parent\n</div>` behind.
        let html = taskify_checklists(&md_to_html("- [ ] parent\n  - [ ] child\n"));
        assert!(
            html.contains(r#"<div><input type="checkbox" contenteditable="false">&nbsp;parent</div>"#),
            "got: {html}"
        );
    }

    #[test]
    fn deep_nesting_clamps_at_the_editor_max_indent() {
        // NoteEditor.svelte:504 clamps indent at 168px. Deeper markdown must
        // flatten onto the last level, not invent an indent the editor can
        // never produce or round-trip.
        let md: String = (0..9)
            .map(|level| format!("{}- [ ] l{level}\n", " ".repeat(level * 2)))
            .collect();
        let html = taskify_checklists(&md_to_html(&md));
        assert!(html.contains("margin-left: 168px"), "got: {html}");
        for px in [196, 224] {
            assert!(
                !html.contains(&format!("margin-left: {px}px")),
                "must clamp, not keep counting past 168px: {html}"
            );
        }
    }

    #[test]
    fn div_style_is_restricted_to_margin_left() {
        // `style` on a div is allowed for ONE thing — the checklist indent.
        // Anything else an agent puts there is dropped, not passed through.
        let clean = sanitize_note_html(
            r#"<div style="margin-left: 28px">in</div><div style="position: fixed; top: 0">out</div>"#,
        );
        assert!(clean.contains(r#"<div style="margin-left: 28px">in</div>"#), "got: {clean}");
        assert!(clean.contains("<div>out</div>"), "arbitrary CSS must not ride in: {clean}");
    }

    #[test]
    fn task_rows_survive_sanitize() {
        // The allowlist must carry contenteditable and margin-left style,
        // or the conversion is undone one step later.
        let out = sanitize_note_html(&taskify_checklists(&md_to_html("- [ ] x\n")));
        assert!(out.contains(r#"contenteditable="false""#), "got: {out}");
    }

    #[test]
    fn agent_written_task_makes_a_note_replace_unsafe() {
        // Deliberate consequence (spec §4.4): once a box exists, someone may tick it.
        let body = sanitize_note_html(&taskify_checklists(&md_to_html("- [ ] x\n")));
        assert!(!is_replace_safe(&body));
    }

    #[test]
    fn write_list_and_replace_guard_disagree_on_checklists() {
        // The asymmetry that IS the two-list split: one body, two verdicts.
        // An agent may WRITE a checkbox (tasklists must round-trip), yet a
        // body already containing one must never be replaced wholesale.
        let html = "<ul><li><input type=\"checkbox\" checked=\"\">done</li></ul>";
        assert_eq!(
            canonicalize_note_html(html),
            sanitize_note_html(html),
            "permissive list must leave a checklist untouched"
        );
        assert!(
            !is_replace_safe(html),
            "strict list must still refuse to replace it"
        );
    }
}
