import type {
  DiagramEdge,
  DiagramPort,
  DiagramReaction,
  DiagramSnapshot,
} from "./protocol";
import { locateStep, quotedPrompt, tokens } from "./step-source.ts";

type Span = { start: number; end: number };
type Edit = Span & { value: string };
type Position = "before" | "after" | "remove";
export type GraphChange = {
  position: Position;
  port: string;
  name: string;
  role: string;
  prompt: string;
  repairs: Record<string, string>;
};

function applyEdits(source: string, edits: Edit[]) {
  let result = source;
  for (const edit of edits.sort((a, b) => b.start - a.start || b.end - a.end)) {
    result = result.slice(0, edit.start) + edit.value + result.slice(edit.end);
  }
  return result;
}

export function localPort(port: DiagramPort, instance: string) {
  return port.name.startsWith(instance + ".")
    ? port.name.slice(instance.length + 1)
    : port.name;
}

// Use declaration indices from the compiler. Rust blocks and prompt literals are opaque tokens.
function declaration(
  source: string,
  snapshot: DiagramSnapshot,
  reaction: DiagramReaction,
) {
  const team =
    snapshot.instances.find(
      (row) => row.id === "instance::" + reaction.instance,
    )?.team ?? snapshot.team;
  const all = tokens(source);
  const header = all.findIndex(
    (t, i) => !t.string && t.text === "team" && all[i + 1]?.text === team,
  );
  if (header < 0) throw Error("This definition is in another source file.");
  const open = all.findIndex(
    (t, i) => i > header && !t.string && t.text === "{",
  );
  const end = all.findIndex((t, i) => i > open && !t.string && t.text === "}");
  const idx = Number(/(?:^|\.)reaction\.(\d+)$/.exec(reaction.name)?.[1]);
  const start = all
    .map((t, i) =>
      i > open &&
      i < end &&
      !t.string &&
      (t.text === "reaction" || t.text === "task")
        ? i
        : -1,
    )
    .filter((i) => i >= 0)[idx];
  if (start === undefined)
    throw Error("Open source to locate this declaration.");
  const step = locateStep(source, snapshot, reaction);
  let last: number;
  let code: (typeof all)[number] | undefined;
  if (all[start].text === "task" && step) last = step.promptSpan.end;
  else if (all[start].text === "reaction") {
    code = all
      .slice(start + 1, end)
      .find((t) => !t.string && t.text === "{=…=}");
    if (!code) throw Error("Could not locate the end of this code reaction.");
    last = code.end;
  } else throw Error("Open source to edit this task's contract.");
  return {
    team,
    step,
    code,
    header: all[header + 1],
    open: all[open],
    headerTokens: all.slice(header + 2, open),
    declarationTokens: all.slice(start, end),
    span: { start: all[start].annotations?.spans[0]?.start ?? all[start].start, end: last },
  };
}

export function graphChange(
  source: string,
  snapshot: DiagramSnapshot,
  reaction: DiagramReaction,
  change: GraphChange,
): string {
  const { step, code, header, headerTokens, declarationTokens, span } =
    declaration(source, snapshot, reaction);
  const portById = (id: string) => {
    const port = snapshot.ports.find((p) => p.id === id);
    if (!port) throw Error("Select a typed port for this operation.");
    return port;
  };
  const ports = snapshot.ports.filter((p) => p.instance === reaction.instance);
  const local = (p: DiagramPort) => localPort(p, reaction.instance);
  const inScope = (p: DiagramPort) => {
    if (p.instance !== reaction.instance)
      throw Error("Open source to edit a connection across team definitions.");
    return local(p);
  };
  const format = (line: string) => "\n    " + line;
  if (change.position === "remove") {
    const connections = reaction.effects.map((id) => {
      const out = portById(id);
      const replacement = ports.find((p) => p.id === change.repairs[id]);
      if (
        !replacement ||
        replacement.type !== out.type ||
        reaction.effects.includes(replacement.id)
      ) {
        throw Error("Choose a compatible replacement for " + out.name + ".");
      }
      return inScope(replacement) + " -> " + inScope(out);
    });
    return applyEdits(source, [
      { ...span, value: connections.join(format("")) },
    ]);
  }
  const selected = portById(change.port);
  const old = inScope(selected);
  const name = change.name.trim();
  if (
    !/^[A-Za-z_][A-Za-z0-9_]*$/.test(name) ||
    ports.some((p) => local(p) === name)
  )
    throw Error("Give the new output a unique identifier.");
  const role = snapshot.agents.find(
    (a) =>
      a.instance === reaction.instance &&
      a.name.split(".").at(-1) === change.role,
  );
  const addReviewer =
    !role &&
    change.role === "reviewer" &&
    !snapshot.agents.some((a) => a.instance === reaction.instance);
  if ((!role && !addReviewer) || !change.prompt.trim())
    throw Error("Choose an execution role and enter its instructions.");
  const before = change.position === "before";
  const candidates = before ? reaction.triggers : reaction.effects;
  if (!candidates.includes(selected.id))
    throw Error("This connection no longer belongs to the selected step.");
  const edits: Edit[] = [];
  if (addReviewer) {
    const bracket = headerTokens.find((t) => t.text === "]");
    if (headerTokens.length && !bracket)
      throw Error("Open source to declare a role for this team.");
    const at = bracket?.start ?? header.end;
    edits.push({
      start: at,
      end: at,
      value: bracket ? "reviewer : Web" : "[reviewer : Web]",
    });
  }
  // Keep downstream consumers and output ports intact. Insert only on the chosen leg.
  if (code) {
    // Keep the original Rust identifiers in scope. The input alias / output
    // assignment adapts the wire without textual replacements inside Rust.
    const close = declarationTokens.findIndex((t) => t.text === ")");
    const fields = before
      ? declarationTokens.slice(2, close)
      : declarationTokens.slice(close + 2, declarationTokens.indexOf(code));
    const field = fields.filter((t) => !t.string && t.text === old);
    if (
      field.length !== 1 ||
      (!before && fields.length !== 1) ||
      old.includes(".")
    )
      throw Error("Open source to split this compound code contract.");
    edits.push({ ...field[0], value: name });
    if (before)
      edits.push({
        start: code.start + 2,
        end: code.start + 2,
        value: "\n        let " + old + " = " + name + ";\n",
      });
    else {
      edits.push({
        start: code.start + 2,
        end: code.start + 2,
        value: "\n        let mut " + old + " = None;\n",
      });
      edits.push({
        start: code.end - 2,
        end: code.end - 2,
        value: "\n        " + name + " = " + old + ";\n    ",
      });
    }
  } else if (!step) throw Error("Open source to locate this task.");
  else if (before) {
    if (!step.triggers.includes(old))
      throw Error("This trigger is not a simple local port.");
    edits.push({
      ...step.triggersSpan,
      value: step.triggers
        .map((trigger) => (trigger === old ? name : trigger))
        .join(", "),
    });
    const prompt = step.prompt.replaceAll("$(" + old + ")", "$(" + name + ")");
    if (prompt !== step.prompt)
      edits.push({ ...step.promptSpan, value: quotedPrompt(prompt) });
  } else {
    if (step.contract.trim() !== old)
      throw Error(
        "Open source to split a conditional or multi-output production contract.",
      );
    edits.push({ ...step.contractSpan, value: name });
    // A task may refer to its declared output by name as part of its instructions.
    const prompt = step.prompt.replaceAll("$(" + old + ")", "$(" + name + ")");
    if (prompt !== step.prompt)
      edits.push({ ...step.promptSpan, value: quotedPrompt(prompt) });
  }
  const offset = before ? span.start : span.end;
  const input = before ? old : name;
  const output = before ? name : old;
  const task =
    "task " +
    change.role +
    "(" +
    input +
    ") -> " +
    output +
    " " +
    quotedPrompt(change.prompt);
  const portDeclaration = "action " + name + " : " + selected.type;
  edits.push({
    start: offset,
    end: offset,
    value: before
      ? portDeclaration + format(task) + format("")
      : format(portDeclaration) + format(task),
  });
  return applyEdits(source, edits);
}

// For instance-to-instance wires, split the actual connection in the enclosing
// definition. An inserted team keeps its own executor and typed input/output.
export function splitConnection(
  source: string,
  snapshot: DiagramSnapshot,
  edge: DiagramEdge,
  change: GraphChange,
): string {
  const from = snapshot.ports.find((p) => p.id === edge.source);
  const to = snapshot.ports.find((p) => p.id === edge.target);
  if (!from || !to || edge.kind !== "connection")
    throw Error("Select a connection between typed ports.");
  const all = tokens(source);
  const blocks = all.flatMap((t, i) =>
    !t.string && (t.text === "main" || t.text === "team") ? [i] : [],
  );
  const owners = ["", ...snapshot.instances.map((inst) => inst.name)].filter(
    (prefix) =>
      !prefix ||
      (from.name.startsWith(prefix + ".") && to.name.startsWith(prefix + ".")),
  );
  const matches: { start: number; end: number; left: string; right: string }[] =
    [];
  for (const owner of owners) {
    const inst = snapshot.instances.find((x) => x.name === owner);
    const left = owner ? from.name.slice(owner.length + 1) : from.name;
    const right = owner ? to.name.slice(owner.length + 1) : to.name;
    for (const i of blocks.filter((i) =>
      inst
        ? all[i].text === "team" && all[i + 1]?.text === inst.team
        : all[i].text === "main",
    )) {
      const open = all.findIndex(
        (t, n) => n >= i && !t.string && t.text === "{",
      );
      const close = all.findIndex(
        (t, n) => n > open && !t.string && t.text === "}",
      );
      const pieces = [
        ...left.split(".").flatMap((x, j) => (j ? [".", x] : [x])),
        "->",
        ...right.split(".").flatMap((x, j) => (j ? [".", x] : [x])),
      ];
      for (let j = open + 1; j + pieces.length <= close; j++) {
        if (
          all[j - 1]?.text === "." ||
          !pieces.every(
            (part, k) => !all[j + k].string && all[j + k].text === part,
          )
        )
          continue;
        if (all[j + pieces.length]?.text !== ".")
          matches.push({
            start: all[j].start,
            end: all[j + pieces.length - 1].end,
            left,
            right,
          });
      }
    }
  }
  if (matches.length !== 1)
    throw Error(
      "This connection cannot be located unambiguously. Open its source.",
    );
  const name = change.name.trim();
  if (
    !/^[A-Za-z_][A-Za-z0-9_]*$/.test(name) ||
    all.some((t) => !t.string && t.text === name)
  )
    throw Error("Choose an unused step name.");
  const team = name + "Step";
  if (all.some((t) => !t.string && t.text === team))
    throw Error("The generated team name is already used.");
  if (
    !["Web", "Codex", "ClaudeCode", "Cursor", "OpenCode", "Pi", "Agy"].includes(
      change.role,
    ) ||
    !change.prompt.trim()
  )
    throw Error("Choose a backend and enter instructions.");
  if (!/^(bool|string|int|float|path|bytes)$/.test(from.type))
    throw Error("Open source to insert a step using this custom type.");
  const wire = matches[0];
  const definition =
    "team " +
    team +
    "[worker : " +
    change.role +
    "] {\n    input value : " +
    from.type +
    "\n    output result : " +
    from.type +
    "\n    task worker(value) -> result " +
    quotedPrompt(change.prompt) +
    "\n}\n\n";
  const first = all[blocks[0]].start;
  return applyEdits(source, [
    { start: first, end: first, value: definition },
    {
      ...wire,
      value:
        name +
        " = " +
        team +
        "()\n    " +
        wire.left +
        " -> " +
        name +
        ".value\n    " +
        name +
        ".result -> " +
        wire.right,
    },
  ]);
}
