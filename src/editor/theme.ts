/**
 * The editor's CodeMirror theme: the terminal's own palette (CLAUDE.md §14).
 * Everything is a `theme.css` variable, so a change in Settings reaches the
 * editor with no re-theming here.
 */

import { HighlightStyle, syntaxHighlighting } from "@codemirror/language";
import type { Extension } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { tags as t } from "@lezer/highlight";

const chrome = EditorView.theme(
  {
    "&": {
      height: "100%",
      color: "var(--fg)",
      backgroundColor: "transparent",
    },
    "&.cm-focused": { outline: "none" },
    ".cm-scroller": {
      fontFamily: "var(--font-family)",
      fontSize: "var(--editor-font-size)",
      lineHeight: "1.5",
      overflow: "auto",
    },
    ".cm-content": { caretColor: "var(--cursor)" },
    ".cm-cursor, .cm-dropCursor": { borderLeftColor: "var(--cursor)" },
    "&.cm-focused > .cm-scroller > .cm-selectionLayer .cm-selectionBackground, .cm-selectionBackground, .cm-content ::selection":
      { backgroundColor: "var(--selection)" },
    ".cm-gutters": {
      backgroundColor: "var(--bg)",
      color: "var(--ansi-8)",
      border: "none",
    },
    ".cm-activeLine": { backgroundColor: "var(--focus-tint)" },
    ".cm-activeLineGutter": {
      backgroundColor: "transparent",
      color: "var(--fg)",
    },
    ".cm-foldPlaceholder": {
      backgroundColor: "transparent",
      border: "none",
      color: "var(--ansi-8)",
    },
    ".cm-matchingBracket, &.cm-focused .cm-matchingBracket": {
      backgroundColor: "var(--selection)",
      color: "inherit",
    },
    ".cm-searchMatch": {
      backgroundColor: "var(--selection)",
      outline: "1px solid var(--divider-active)",
    },
    ".cm-searchMatch.cm-searchMatch-selected": {
      backgroundColor: "var(--divider-active)",
    },
    ".cm-selectionMatch": { backgroundColor: "var(--selection)" },
    ".cm-panels": {
      backgroundColor: "var(--bg)",
      color: "var(--fg)",
      fontFamily: "var(--font-family)",
    },
    ".cm-panels.cm-panels-bottom": {
      borderTop: "var(--divider-size) solid var(--divider)",
    },
    ".cm-panels.cm-panels-top": {
      borderBottom: "var(--divider-size) solid var(--divider)",
    },
    ".cm-panel.cm-search input.cm-textfield": {
      backgroundColor: "transparent",
      color: "var(--fg)",
      border: "1px solid var(--divider)",
      borderRadius: "0",
    },
    ".cm-panel.cm-search input.cm-textfield:focus": {
      borderColor: "var(--divider-active)",
      outline: "none",
    },
    ".cm-panel.cm-search button": {
      backgroundImage: "none",
      backgroundColor: "transparent",
      color: "var(--fg)",
      border: "1px solid var(--divider)",
      borderRadius: "0",
    },
    ".cm-tooltip": {
      backgroundColor: "var(--bg)",
      color: "var(--fg)",
      border: "1px solid var(--divider)",
    },
    ".cm-tooltip-autocomplete ul li[aria-selected]": {
      backgroundColor: "var(--selection)",
      color: "var(--fg)",
    },
  },
  { dark: true },
);

const syntax = HighlightStyle.define([
  { tag: [t.comment, t.meta], color: "var(--ansi-8)", fontStyle: "italic" },
  {
    tag: [t.keyword, t.modifier, t.operatorKeyword, t.controlKeyword],
    color: "var(--ansi-5)",
  },
  { tag: [t.string, t.special(t.string), t.regexp], color: "var(--ansi-2)" },
  { tag: [t.number, t.bool, t.null, t.atom], color: "var(--ansi-3)" },
  { tag: [t.typeName, t.className, t.namespace], color: "var(--ansi-6)" },
  {
    tag: [t.function(t.variableName), t.function(t.propertyName), t.macroName],
    color: "var(--ansi-4)",
  },
  { tag: [t.tagName, t.heading], color: "var(--ansi-1)", fontWeight: "bold" },
  { tag: [t.attributeName, t.labelName], color: "var(--ansi-3)" },
  { tag: t.link, color: "var(--ansi-4)", textDecoration: "underline" },
  { tag: t.emphasis, fontStyle: "italic" },
  { tag: t.strong, fontWeight: "bold" },
  { tag: t.invalid, color: "var(--ansi-1)" },
]);

export const editorTheme: Extension = [chrome, syntaxHighlighting(syntax)];
