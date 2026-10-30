"use client";

import { useEffect, useMemo, useRef, useState } from "react";
import { Annotation, Compartment, EditorState, RangeSetBuilder } from "@codemirror/state";
import { Decoration, EditorView, ViewPlugin, keymap, type DecorationSet, type ViewUpdate } from "@codemirror/view";
import { defaultKeymap, history, historyKeymap, indentWithTab } from "@codemirror/commands";
import { getChunks, unifiedMergeView } from "@codemirror/merge";
import { tokenizeOmar, type TokenKind } from "./lib/omar-syntax";

/** Highlighted OMAR source. Tokenizing is pure, so it is memoised per program. */
export function OmarSource({ source }: { source: string }) {
  const tokens = useMemo(() => tokenizeOmar(source), [source]);
  return (
    <pre className="source-code">
      <code>
        {tokens.map((token, index) => (
          <span key={index} className={`omar-tok-${token.kind}`}>
            {token.text}
          </span>
        ))}
      </code>
    </pre>
  );
}

const marks = new Map<TokenKind, Decoration>();

/** The OMAR tokenizer's colours, as editor decorations. */
function highlight(state: EditorState): DecorationSet {
  const builder = new RangeSetBuilder<Decoration>();
  let at = 0;
  for (const token of tokenizeOmar(state.doc.toString())) {
    const end = at + token.text.length;
    if (token.kind !== "text" && end > at) {
      let mark = marks.get(token.kind);
      if (!mark) marks.set(token.kind, (mark = Decoration.mark({ class: `omar-tok-${token.kind}` })));
      builder.add(at, end, mark);
    }
    at = end;
  }
  return builder.finish();
}

const highlighting = ViewPlugin.fromClass(
  class {
    decorations: DecorationSet;
    constructor(view: EditorView) {
      this.decorations = highlight(view.state);
    }
    update(update: ViewUpdate) {
      if (update.docChanged) this.decorations = highlight(update.state);
    }
  },
  { decorations: (plugin) => plugin.decorations },
);

/** Marks a change that came from the props rather than the operator. */
const fromProps = Annotation.define<boolean>();

/** The diff against `base`, review style: removed lines in red above what
    replaced them, added lines in green. Nothing without a base. */
function diffAgainst(base: string | null) {
  return base === null
    ? []
    : unifiedMergeView({ original: base, mergeControls: false, gutter: false, syntaxHighlightDeletions: false });
}

/** Bring the first change into view: a change that arrives — proposed, or
    the base it is read against — is what the operator has to look at. */
function revealFirstChange(editor: EditorView) {
  const first = getChunks(editor.state)?.chunks[0];
  if (!first) return;
  editor.dispatch({
    effects: EditorView.scrollIntoView(Math.min(first.fromB, editor.state.doc.length), { y: "center" }),
  });
}

/**
 * The same source, editable, checked by the compiler as it is typed.
 *
 * Given a `base` — the program a live run executes — the editor shows the
 * edit as a review-style diff against it, updated as the operator types, so
 * what a change would do to the run is read in the code itself.
 *
 * Checking is the runtime's own compiler and verifier rather than an
 * approximation of them here: a program this calls valid is one the daemon
 * accepts, and there is no second opinion to keep in step.
 */
export function OmarEditor({
  source,
  base = null,
  filename,
  status,
  errors,
  checking,
  readOnly = false,
  onSourceChange,
  onFilenameChange,
}: {
  source: string;
  /** What the source is compared against; `null` shows it plainly. */
  base?: string | null;
  filename: string;
  /** What the run is doing, or "draft" before there is one. */
  status: string;
  errors: string[];
  checking: boolean;
  /** Shown, highlighted and scrollable, but not editable. */
  readOnly?: boolean;
  onSourceChange: (source: string) => void;
  onFilenameChange: (filename: string) => void;
}) {
  const host = useRef<HTMLDivElement | null>(null);
  const view = useRef<EditorView | null>(null);
  const diff = useRef(new Compartment());
  const onChange = useRef(onSourceChange);
  useEffect(() => {
    onChange.current = onSourceChange;
  });
  // The first values, for the editor's creation; later ones arrive below.
  const initial = useRef({ source, base, readOnly });
  // Null until the operator types: the name shown is the one they were given
  // until it is the one they are writing. Derived rather than synchronised, so
  // a rename in progress is never overwritten by a re-render.
  const [edited, setEdited] = useState<string | null>(null);
  const name = edited ?? filename;

  useEffect(() => {
    const editor = new EditorView({
      parent: host.current!,
      state: EditorState.create({
        doc: initial.current.source,
        extensions: [
          history(),
          // In a code pane Tab indents rather than leaving the editor.
          keymap.of([indentWithTab, ...defaultKeymap, ...historyKeymap]),
          EditorView.lineWrapping,
          highlighting,
          EditorView.contentAttributes.of({ "aria-label": "OMAR program", spellcheck: "false" }),
          EditorView.updateListener.of((update) => {
            if (update.docChanged && !update.transactions.some((tr) => tr.annotation(fromProps))) {
              onChange.current(update.state.doc.toString());
            }
          }),
          diff.current.of(diffAgainst(initial.current.base)),
          EditorState.readOnly.of(initial.current.readOnly),
          EditorView.editable.of(!initial.current.readOnly),
        ],
      }),
    });
    view.current = editor;
    return () => editor.destroy();
  }, []);

  useEffect(() => {
    const editor = view.current;
    if (!editor || editor.state.doc.toString() === source) return;
    editor.dispatch({
      changes: { from: 0, to: editor.state.doc.length, insert: source },
      annotations: fromProps.of(true),
    });
    revealFirstChange(editor);
  }, [source]);

  useEffect(() => {
    const editor = view.current;
    if (!editor) return;
    editor.dispatch({ effects: diff.current.reconfigure(diffAgainst(base)) });
    revealFirstChange(editor);
  }, [base]);

  function commit() {
    setEdited(null);
    onFilenameChange(name);
  }

  return (
    <>
      <div className="source-title">
        <input
          className="source-name"
          value={name}
          spellCheck={false}
          readOnly={readOnly}
          aria-label="Program file name"
          onChange={(event) => setEdited(event.target.value)}
          onBlur={commit}
          onKeyDown={(event) => {
            if (event.key === "Enter") {
              event.preventDefault();
              commit();
              (event.target as HTMLInputElement).blur();
            }
          }}
        />
        <span>{checking ? "checking…" : status}</span>
      </div>

      <div className="source-editor" ref={host} />

      {errors.length > 0 ? (
        <div className="source-errors" role="alert">
          {errors.map((error) => (
            <p key={error}>{error}</p>
          ))}
        </div>
      ) : null}
    </>
  );
}
