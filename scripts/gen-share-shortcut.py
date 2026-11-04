#!/usr/bin/env python3
"""Generate the "Share to Jodd" macOS Shortcut (Share to Jodd spec §3.6).

    python3 scripts/gen-share-shortcut.py /tmp/unsigned.shortcut
    shortcuts sign --mode anyone --input /tmp/unsigned.shortcut \
        --output "src-tauri/assets/Share to Jodd.shortcut"

Three actions: URL-encode the Shortcut input (a Safari page, a URL or text —
coerced to text, so a page becomes its URL), put it after
`jodd://capture?text=`, open that URL. Rust finds the link inside the text
(`capture::links`). See WFWorkflowTypes below for where it appears.
`capture_commands::tests::the_bundled_share_shortcut_is_signed` fails if an
unsigned file is committed.
"""
import plistlib
import sys
import uuid

A, B = str(uuid.uuid4()).upper(), str(uuid.uuid4()).upper()
PREFIX = "jodd://capture?text="

workflow = {
    "WFWorkflowMinimumClientVersion": 900,
    "WFWorkflowMinimumClientVersionString": "900",
    "WFWorkflowClientVersion": "2607.0.2",
    "WFWorkflowIcon": {"WFWorkflowIconStartColor": 463140863, "WFWorkflowIconGlyphNumber": 61440},
    "WFWorkflowImportQuestions": [],
    # ActionExtension = "Show in Share Sheet"; QuickActions + the Services
    # surface = "Use as Quick Action → Services Menu", i.e. right-click →
    # Services → Share to Jodd on selected text in any app (Chrome included).
    "WFWorkflowTypes": ["ActionExtension", "QuickActions"],
    "WFQuickActionSurfaces": ["Services"],
    "WFWorkflowHasShortcutInputVariables": True,
    "WFWorkflowInputContentItemClasses": [
        "WFURLContentItem", "WFSafariWebPageContentItem", "WFArticleContentItem",
        "WFStringContentItem", "WFRichTextContentItem",
    ],
    # NO WFWorkflowOutputContentItemClasses key, not even empty: with it,
    # Shortcuts appends "Stop and output" on import and turns Provide Output
    # on (measured 2026-10-06) — and a Services quick action that provides
    # output REPLACES the selected text in an editable field.
    "WFWorkflowActions": [
        {"WFWorkflowActionIdentifier": "is.workflow.actions.urlencode",
         "WFWorkflowActionParameters": {
             "UUID": A, "WFEncodeMode": "Encode",
             # A TEXT parameter takes a token STRING. As a bare
             # WFTextTokenAttachment (what Open's content parameter takes) the
             # editor showed an empty "Text" placeholder and the shortcut
             # encoded nothing — measured 2026-10-06 via Services.
             "WFInput": {"Value": {"string": "\ufffc",
                                   "attachmentsByRange": {"{0, 1}": {"Type": "ExtensionInput"}}},
                         "WFSerializationType": "WFTextTokenString"}}},
        {"WFWorkflowActionIdentifier": "is.workflow.actions.gettext",
         "WFWorkflowActionParameters": {
             "UUID": B,
             "WFTextActionText": {
                 "Value": {"string": PREFIX + "￼",
                           "attachmentsByRange": {"{%d, 1}" % len(PREFIX): {
                               "OutputName": "URL Encoded Text", "OutputUUID": A, "Type": "ActionOutput"}}},
                 "WFSerializationType": "WFTextTokenString"}}},
        {"WFWorkflowActionIdentifier": "is.workflow.actions.openurl",
         "WFWorkflowActionParameters": {
             "WFInput": {"Value": {"OutputName": "Text", "OutputUUID": B, "Type": "ActionOutput"},
                         "WFSerializationType": "WFTextTokenAttachment"}}},
    ],
}

with open(sys.argv[1], "wb") as f:
    plistlib.dump(workflow, f, fmt=plistlib.FMT_BINARY)
