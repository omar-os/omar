"use client";

import { FormEvent, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ChatMessage as ChatMessageView } from "./chat-message";
import { ChatHistory, OmarLogo, SidebarIcon, useHistoryDrawer } from "./chat-history";
import { AgentTerminal } from "./agent-terminal";
import { Timeline } from "./timeline";
import { BackendMenu } from "./backend-menu";
import { DiagramCanvas } from "./diagram/diagram-canvas";
import { OmarEditor } from "./omar-source";
import { PortPanel } from "./port-panel";
import { Resizer } from "./resizer";
import { Waiting } from "./waiting";
import { Suggestions } from "./suggestions";
import { fetchDecisionCapabilities } from "./lib/decision-client";
import {
  eaDesignAgent,
  scriptedDesignAgent,
  type DesignAgent,
} from "./lib/design-agent";
import { reviewProgram, reviewWorkflow } from "./lib/fixtures";
import {
  applyDiagramEvent,
  formatDuration,
  isRunFinished,
  openInputs,
  type ChatMessage,
  type ConversationSummary,
  type DiagramEvent,
  type DiagramSnapshot,
  type PendingInvocation,
  type ProposedDesign,
  type RunRecord,
  type DecisionCapabilities,
} from "./lib/protocol";
import type { TimelineStep } from "./lib/runtime-client";
import {
  answerPanel,
  ASSISTANT,
  checkServeHealth,
  diagramUrlFor,
  fetchDiagram,
  fetchPanel,
  fetchRun,
  checkProgram,
  startRun,
  stopRun,
  subscribeToDiagram,
  fetchConversations,
} from "./lib/runtime-client";

/**
 * The operator's position in the flow. A design is never executed without
 * passing through `review`, which is the confirmation gate.
 */
type Phase =
  | "idle"
  | "drafting"
  | "review"
  | "spawning"
  | "observing"
  | "finished"
  | "failed";

/** Whether `omar serve` is reachable, tracked separately from the flow phase. */
type Daemon =
  | { state: "demo" }
  | { state: "checking" }
  | { state: "live" }
  | { state: "offline"; reason: string };

const HEALTH_POLL_MS = 5000;
/**
 * Column bounds. A panel dragged below its minimum collapses to nothing rather
 * than lingering unusably narrow; its divider becomes the control to bring it
 * back, which is why there are no separate show/hide buttons.
 */
const MIN_BUILDER = 300;
const MIN_INSPECTOR = 260;
const MIN_DIAGRAM = 320;
const DEFAULT_BUILDER = 380;
const DEFAULT_INSPECTOR = 400;
/** Drag past this fraction of a panel's minimum and it collapses. */
const COLLAPSE_AT = 0.6;

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

type StudioProps = { serveUrl?: string; designAgent?: DesignAgent };

export function Studio({ serveUrl = "", designAgent }: StudioProps) {
  const [selected, setSelected] = useState<string | null>(null);
  const select = useCallback((conversation: ConversationSummary) => {
    setSelected(conversation.id);
    sessionStorage.setItem(`omar-chat:${serveUrl}`, conversation.id);
  }, [serveUrl]);
  useEffect(() => {
    if (!serveUrl) return;
    const saved = sessionStorage.getItem(`omar-chat:${serveUrl}`);
    if (!saved) return;
    const abort = new AbortController();
    void fetchConversations(serveUrl, abort.signal).then((history) => {
      if (!abort.signal.aborted && history.conversations.some((chat) => chat.id === saved)) setSelected(saved);
    }).catch(() => {});
    return () => abort.abort();
  }, [serveUrl]);
  const scopedUrl = selected ? `${serveUrl.replace(/\/$/, "")}/chats/${encodeURIComponent(selected)}` : serveUrl;
  return <StudioWorkspace serveUrl={scopedUrl} historyUrl={serveUrl}
    designAgent={designAgent} selectedId={selected} onSelect={select} />;
}

function StudioWorkspace({ serveUrl = "", historyUrl, designAgent, selectedId, onSelect }: StudioProps & {
  historyUrl: string;
  selectedId: string | null;
  onSelect: (conversation: ConversationSummary) => void;
}) {
  const isDemo = serveUrl.trim().length === 0;
  // Requests from a previous selection must not update the retained workspace.
  const scopeRef = useRef(0);
  useEffect(() => {
    scopeRef.current += 1;
    return () => { scopeRef.current += 1; };
  }, [serveUrl]);
  const [snapshot, setSnapshot] = useState<DiagramSnapshot | null>(
    isDemo ? reviewWorkflow : null,
  );
  const [source, setSource] = useState(isDemo ? reviewProgram : "");
  /** What the program is called. Named for the team it declares until the
      operator says otherwise. */
  const [filename, setFilename] = useState(
    isDemo ? `${reviewWorkflow.team}.omar` : "program.omar",
  );
  /** What the compiler said about the source as it stands. */
  const [sourceErrors, setSourceErrors] = useState<string[]>([]);
  const [checking, setChecking] = useState(false);
  /** Every tag the program passes through, projected or observed. */
  const [steps, setSteps] = useState<TimelineStep[]>([]);
  const [truncated, setTruncated] = useState(false);
  const [stepIndex, setStepIndex] = useState(0);
  const [timelineOpen, setTimelineOpen] = useState(false);
  /** Following the run rather than being scrubbed by hand. */
  const [following, setFollowing] = useState(true);
  /** Ports the operator has set on the live run. */
  // can flip back and forth once there is.
  const [tab, setTab] = useState<"source" | "events" | "suggestions">("source");
  const [decisionCapabilities, setDecisionCapabilities] = useState<DecisionCapabilities | null>(null);
  /** What the run's web agents are waiting to be given. */
  const [pending, setPending] = useState<PendingInvocation[]>([]);
  const [answering, setAnswering] = useState(false);
  const [builderWidth, setBuilderWidth] = useState(DEFAULT_BUILDER);
  const [inspectorWidth, setInspectorWidth] = useState(DEFAULT_INSPECTOR);
  // The first design splits the window down the middle. After that the widths
  // are the operator's, so this only ever fires once.
  const arrangedRef = useRef(isDemo);
  const workspaceRef = useRef<HTMLElement | null>(null);
  const dragOriginRef = useRef({
    builder: DEFAULT_BUILDER,
    inspector: DEFAULT_INSPECTOR,
  });
  // Deploying starts real agents, so the button arms a second, explicit step.
  const [confirming, setConfirming] = useState(false);
  const [daemon, setDaemon] = useState<Daemon>(
    isDemo ? { state: "demo" } : { state: "checking" },
  );
  const [phase, setPhase] = useState<Phase>("idle");
  const [assistantBusy, setAssistantBusy] = useState(false);
  const [selectingChat, setSelectingChat] = useState(false);
  const [hydratedUrl, setHydratedUrl] = useState(isDemo ? serveUrl : null);
  const switchingChat = selectingChat || hydratedUrl !== serveUrl;
  const [design, setDesign] = useState<ProposedDesign | null>(null);
  const [run, setRun] = useState<RunRecord | null>(null);
  const [error, setError] = useState("");
  const [prompt, setPrompt] = useState("");
  /** Diagram components the operator has highlighted for the next message. */
  const [selection, setSelection] = useState<string[]>([]);
  /** The agent whose terminal is open, if any. */
  const [terminalAgent, setTerminalAgent] = useState<string | null>(null);
  const [historyOpen, setHistoryOpen] = useState(true);
  const [drawerOpen, setDrawerOpen] = useState(false);
  const [historyRevision, setHistoryRevision] = useState(0);
  const historyButtonRef = useRef<HTMLButtonElement>(null);
    const historyRailButtonRef = useRef<HTMLButtonElement>(null);
  const historyDrawer = useHistoryDrawer();
  useEffect(() => {
    if (historyDrawer && !switchingChat && serveUrl !== historyUrl) historyButtonRef.current?.focus();
  }, [historyDrawer, serveUrl, historyUrl, switchingChat]);
  const historyVisible = !isDemo && (historyDrawer ? drawerOpen : historyOpen);
  function closeHistory() {
    if (historyDrawer) setDrawerOpen(false);
    else setHistoryOpen(false);
    historyButtonRef.current?.focus();
  }
    function openHistory() {
    if (historyDrawer) setDrawerOpen(true);
    else setHistoryOpen(true);
  }
  useEffect(() => {
    if (!isDemo && !historyDrawer && !historyOpen) historyRailButtonRef.current?.focus();
  }, [historyDrawer, historyOpen, isDemo]);

  const conversationIdRef = useRef<string | null>(null);
  const [conversationId, setConversationId] = useState("");
  const [conversationTitle, setConversationTitle] = useState("What should the team do?");
  /** The web agent whose port panel is open. A program may declare several,
      each with its own ports and prompts, so this names one rather than
      merging them into a shared view. */
  const [panelAgent, setPanelAgent] = useState<string | null>(null);

  /**
   * Which inputs the projection should treat as arriving.
   *
   * A closed loop has none, and `project` seeds every timer regardless — so a
   * timer-driven program projects correctly from an empty set. What is left
   * here is the dangling case: a port nothing writes would never arrive on its
   * own, and the question the timeline answers is what the program would do if
   * it did.
   */
  const present = useMemo(
    () => (snapshot ? openInputs(snapshot).map((port) => port.name) : []),
    [snapshot],
  );
  /**
   * The same list as a value rather than an identity.
   *
   * `present` is derived from the snapshot, and the projection sets the
   * snapshot — so depending on the array itself means every projection asks
   * for another one, forever. What matters is which ports are in it.
   */
  const presentKey = present.join("\u0000");
  /**
   * Which check is the current one.
   *
   * Aborting a request tells the network to stop; it does not decide whether a
   * reply that already arrived is still wanted. A reply is applied when it is
   * the newest one asked for, which is the actual question.
   */
  const checkTokenRef = useRef(0);
  // Read by the check, which must not re-run every time a run advances.
  const runRef = useRef<RunRecord | null>(null);
  useEffect(() => {
    runRef.current = run;
  }, [run]);

  /**
   * What the tag being shown touches: the ports carrying a value and the
   * reactions firing. Ids, because that is what the drawing is keyed by.
   */
  const highlighted = useMemo(() => {
    const step = steps[stepIndex];
    if (!step || !snapshot) return new Set<string>();
    // Looked up rather than built: an id is not its name with a prefix on it —
    // a reaction's id carries the instance its name may not — and guessing the
    // shape would light nothing while looking like it worked.
    const lit = new Set<string>();
    const mark = (
      entities: { id: string; name: string }[],
      names: string[],
    ) => {
      for (const name of names) {
        const found = entities.find((entity) => entity.name === name);
        if (found) lit.add(found.id);
      }
    };
    mark(snapshot.ports, step.events);
    mark(snapshot.timers, step.events);
    mark(snapshot.reactions, step.reactions);
    return lit;
  }, [steps, stepIndex, snapshot]);
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [events, setEvents] = useState<DiagramEvent[]>([]);
  const disconnectRef = useRef<null | (() => void)>(null);
  const threadRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => () => disconnectRef.current?.(), []);

  // Poll the daemon so the indicator reflects reality rather than whatever was
  // true when the page loaded.
  useEffect(() => {
    if (isDemo) return;
    let cancelled = false;
    const probe = async () => {
      const health = await checkServeHealth(serveUrl);
      if (cancelled) return;
      setDaemon(health.ok ? { state: "live" } : { state: "offline", reason: health.reason });
    };
    void probe();
    const timer = setInterval(() => void probe(), HEALTH_POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [isDemo, serveUrl]);

  // Older daemons answer 404. Treat that as an absent feature so Mission
  // Control remains fully usable during a rolling upgrade.
  useEffect(() => {
    if (isDemo || !serveUrl) return;
    let cancelled = false;
    void fetchDecisionCapabilities(serveUrl)
      .then((capabilities) => { if (!cancelled) setDecisionCapabilities(capabilities); })
      .catch(() => { if (!cancelled) setDecisionCapabilities(null); });
    return () => { cancelled = true; };
  }, [isDemo, serveUrl]);

  // One agent for the lifetime of a mode. Demo mode never reaches the network.
  const agent = useMemo(
    () => designAgent ?? (isDemo ? scriptedDesignAgent() : eaDesignAgent(serveUrl)),
    [designAgent, isDemo, serveUrl],
  );

  const restoreConversation = useCallback((conversation: ConversationSummary) => {
    setHistoryRevision((current) => current + 1);
    setConversationTitle(conversation.message_count ? conversation.title : "What should the team do?");
    if (conversationIdRef.current === conversation.id) return;
    conversationIdRef.current = conversation.id;
    setConversationId(conversation.id);
    disconnectRef.current?.();
    disconnectRef.current = null;
    checkTokenRef.current += 1;
    runRef.current = null;
    setMessages([]);
    setRun(null);
    setAnswering(false);
    setChecking(false);
    setEvents([]);
    setPending([]);
    setDesign(null);
    setSnapshot(null);
    setSource("");
    setFilename("program.omar");
    setSourceErrors([]);
    setSelection([]);
    setConfirming(false);
    setPhase("idle");
    setPrompt("");
    setError("");
    setTerminalAgent(null);
    setPanelAgent(null);
    setSteps([]);
    setStepIndex(0);
    setTimelineOpen(false);
    setTab("source");
    // Keep the operator's split widths when replacing one topology with another.
  }, []);

  useEffect(() => {
    const thread = threadRef.current;
    if (thread) thread.scrollTop = thread.scrollHeight;
  }, [messages, phase]);

  async function submitPrompt(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const request = prompt.trim();
    if (!request || (switchingChat || (!isDemo && !conversationId)) || assistantBusy || phase === "spawning") return;
    const selected = selection;
    setPrompt("");
    const scope = scopeRef.current;
    setError("");
    // The selection belonged to the message just sent. Keeping it would
    // silently attach it to the next one too.
    setSelection([]);
    setAssistantBusy(true);
    if (!run || isRunFinished(run.status)) setPhase("drafting");
    try {
      await agent.send(request, selected);
    } catch (cause) {
      if (scope !== scopeRef.current) return;
      setError(cause instanceof Error ? cause.message : String(cause));
      setAssistantBusy(false);
      setPhase(run && !isRunFinished(run.status) ? "observing" : "idle");
    }
  }

  /** What the run's web agents are waiting on, as of now. */
  const loadPanel = useCallback(
    async (runId: string) => {
      try {
        const next = await fetchPanel(serveUrl, runId);
        if (runRef.current?.run_id === runId) setPending(next);
      } catch {
        // A panel that cannot be read is not a run that has failed. The next
        // event asks again.
      }
    },
    [serveUrl],
  );

  /** Answer one invocation, as one completion. */
  async function answer(
    invocation: PendingInvocation,
    values: Record<string, unknown>,
  ) {
    if (!run) return;
    setAnswering(true);
    const scope = scopeRef.current;
    setError("");
    try {
      await answerPanel(serveUrl, run.run_id, {
        invocation_id: invocation.invocation_id,
        agent: invocation.agent,
        values,
      });
      if (scope !== scopeRef.current) return;
      await loadPanel(run.run_id);
    } catch (cause) {
      if (scope !== scopeRef.current) return;
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      if (scope === scopeRef.current) setAnswering(false);
    }
  }

  /** Watch a live run until it reaches a terminal status. */
  const observe = useCallback(
    (record: RunRecord) => {
      const diagramUrl = diagramUrlFor(record);
      disconnectRef.current?.();
      let connected = true;

      const refresh = async () => {
        const next = await fetchDiagram(diagramUrl);
        if (connected) setSnapshot(next);
      };
      void refresh().catch(() => {
        /* the SSE stream reports connection loss */
      });

      /**
       * The stream is authoritative for *that* the run ended; serve is
       * authoritative for the recorded status, which it writes only once
       * `run_topology` returns. Poll briefly so the two can't disagree.
       */
      const settle = async () => {
        for (let attempt = 0; attempt < 10; attempt += 1) {
          const latest = await fetchRun(serveUrl, record.run_id).catch(() => null);
          if (!connected) return;
          if (latest) {
            setRun(latest);
            if (isRunFinished(latest.status)) {
              // `stopped` is the ending a stop asked for, so it reads as
              // finished; only `failed` is a failure.
              setPhase(latest.status === "failed" ? "failed" : "finished");
              if (latest.error) setError(latest.error);
              return;
            }
          }
          await new Promise((resolve) => setTimeout(resolve, 300));
        }
      };

      const unsubscribe = subscribeToDiagram(
        diagramUrl,
        (event) => {
          if (!connected) return;
          setEvents((current) => [event, ...current].slice(0, 8));
          // A live run walks the same list the projection drew. Matched on the
          // tag rather than counted, so a run that reaches a tag the projection
          // did not expect leaves the strip where it was rather than lying
          // about where the run is.
          if (event.kind === "tag_advanced" && event.tag) {
            const tag = event.tag;
            setSteps((current) => {
              const at = current.findIndex(
                (step) =>
                  step.timestamp === tag.timestamp && step.microstep === tag.microstep,
              );
              if (at >= 0) setStepIndex(at);
              return current;
            });
          }
          // Apply first, then refetch. The diagram server shuts down with the
          // run, so the fetch that follows the closing events often loses and
          // the picture would otherwise keep a reaction painted as running.
          setSnapshot((current) =>
            current ? applyDiagramEvent(current, event) : current,
          );
          void refresh().catch(() => {});
          // A reaction starting is how a client learns a web agent is waiting:
          // the event carries an id, so the detail is fetched rather than
          // pushed. Completion is when to look again -- an answered invocation
          // leaves the queue the same way.
          if (event.kind === "reaction_started" || event.kind === "reaction_completed") {
            void loadPanel(record.run_id);
          }
          if (event.kind === "run_completed") {
            setPhase("finished");
            // The run's invocation service goes with it, so nothing is owed
            // any more whatever the last fetch saw.
            setPending([]);
            void settle();
          }
          if (event.kind === "run_failed") {
            setPhase("failed");
            const message = (event.payload as { message?: unknown }).message;
            setError(typeof message === "string" ? message : "The run failed.");
            void settle();
          }
        },
        // The per-run diagram server dies with the run, so a dropped stream is
        // expected at the end rather than an error. Ask serve what happened.
        () => void settle(),
      );
      disconnectRef.current = () => {
        connected = false;
        unsubscribe();
      };
    },
    [serveUrl, loadPanel],
  );

  // A chat can be reopened during admission, before its diagram binds.
  useEffect(() => {
    if (switchingChat || !run || run.diagram_address || isRunFinished(run.status)) return;
    const abort = new AbortController();
    const poll = async () => {
      try {
        const latest = await fetchRun(serveUrl, run.run_id, abort.signal);
        if (abort.signal.aborted) return;
        runRef.current = latest;
        setRun(latest);
        if (isRunFinished(latest.status)) {
          setPhase(latest.status === "failed" ? "failed" : "finished");
          if (latest.error) setError(latest.error);
        } else if (latest.diagram_address) {
          observe(latest);
          void loadPanel(latest.run_id);
        }
      } catch { /* daemon health reports connectivity */ }
    };
    const timer = setInterval(() => void poll(), 500);
    return () => { abort.abort(); clearInterval(timer); };
  }, [run, serveUrl, observe, loadPanel, switchingChat]);

  // The conversation is owned by the runtime, not this component: the stream
  // replays history on connect, so a reload rejoins rather than starting over.
  useEffect(() => {
    let connected = true;
    let revision = 0;
    const abort = new AbortController();
    let subscribedId: string | undefined;
    let replaying = false;
    let replayedProposal = false;
    let restoring = false;
    let buffered: ChatMessage[] = [];
    const applyMessage = (message: ChatMessage) => {
      if (message.role === "operator") {
        setAssistantBusy(true);
        setConversationTitle((title) => title === "What should the team do?" ? message.text.replace(/\s+/g, " ").slice(0, 80) : title);
      }
      setMessages((current) =>
        current.some((seen) => seen.sequence === message.sequence)
          ? current
          : [...current, message],
      );
      if (message.role === "assistant" && !message.progress) setAssistantBusy(false);
      if (!message.design) {
        // Only an assistant reply ends the wait — the operator's own message
        // echoes back off the same stream, and commentary while it works is
        // the opposite of finishing. And a reply must not withdraw a pending
        // proposal: assistants routinely comment straight after proposing
        // ("…is in your queue"), which was retracting the gate.
        if (message.role === "assistant" && !message.progress) {
          setPhase((current) => (current === "drafting" ? "idle" : current));
        }
        return;
      }
      if (replaying) replayedProposal = true;
      // A fresh proposal remains in the transcript while this chat's
      // deployed topology continues to own the live diagram and controls.
      if (!replaying && runRef.current && !isRunFinished(runRef.current.status)) return;
      setConfirming(false);
      setDesign(message.design);
      setSource(message.design.program);
      setSourceErrors([]);
      setFilename(`${message.design.preview.team}.omar`);
      // Show the proposed topology, not whatever was on screen before.
      setSnapshot(message.design.preview);
      if (!arrangedRef.current) {
        arrangedRef.current = true;
        const available = workspaceRef.current?.clientWidth ?? 0;
        // Conversation and diagram side by side; the source pane starts
        // collapsed behind its handle rather than crowding the first look.
        if (available) setBuilderWidth(Math.round(available / 2));
        setInspectorWidth(0);
      }
      setPhase(runRef.current && !isRunFinished(runRef.current.status) ? "observing" : "review");
    };
    const unsubscribe = agent.subscribe(
      (message) => {
        if (!connected) return;
        if (restoring) buffered.push(message);
        else if (!subscribedId || subscribedId === conversationIdRef.current) applyMessage(message);
      },
      () => {
        /* daemon health is polled separately */
      },
      (conversation) => {
        revision += 1;
        subscribedId = conversation.id;
        restoring = true;
        buffered = [];
      },
      (conversation) => {
        if (!connected || conversation.id !== subscribedId) return;
        const currentRevision = ++revision;
        const replay = buffered;
        buffered = [];
        const restore = async () => {
          const activeRun = conversation.run && !isRunFinished(conversation.run.status);
          // Keep the current chat on screen while its replacement is replayed
          // and its live snapshot is fetched. Commit them in one React batch.
          const liveSnapshot = activeRun && conversation.run?.diagram_address
            ? await fetchDiagram(diagramUrlFor(conversation.run), abort.signal).catch(() => null)
            : null;
          if (!connected || currentRevision !== revision || conversation.id !== subscribedId) return;
          restoreConversation(conversation);
          runRef.current = conversation.run ?? null;
          setRun(conversation.run ?? null);
          replaying = true;
          replayedProposal = false;
          for (const message of replay) applyMessage(message);
          replaying = false;
          if (liveSnapshot) setSnapshot(liveSnapshot);
          setHydratedUrl(serveUrl);
          if (serveUrl === historyUrl) onSelect(conversation);
          if (conversation.run) {
            const record = conversation.run;
            runRef.current = record;
            setRun(record);
            setConfirming(false);
            setPhase(isRunFinished(record.status) ? (record.status === "failed" ? "failed" : "finished") : "observing");
            setTab("events");
            if (!isRunFinished(record.status) && record.diagram_address) {
              observe(record);
              void loadPanel(record.run_id);
            }
          }
          setAssistantBusy(conversation.busy);
          // A finished run must not override the proposal restored from the
          // transcript. Only a live topology owns the diagram and its controls.
          if (replayedProposal && !activeRun) {
            runRef.current = null;
            setRun(null);
            setTab("source");
            setPhase("review");
          } else if (conversation.busy && !activeRun) {
            setPhase("drafting");
          }
          // Replies received during the snapshot fetch are newer than the
          // replay's state marker; apply them last so busy/proposals stay fresh.
          const liveMessages = buffered;
          buffered = [];
          restoring = false;
          for (const message of liveMessages) applyMessage(message);
        };
        void restore();
      },
    );
    return () => {
      connected = false;
      abort.abort();
      unsubscribe();
      disconnectRef.current?.();
      disconnectRef.current = null;
    };
  }, [agent, restoreConversation, observe, loadPanel, serveUrl, historyUrl, onSelect]);

  async function confirmDesign() {
    if (!design || phase !== "review") return;
    setConfirming(false);
    setPhase("spawning");
    const scope = scopeRef.current;
    setError("");
    try {
      // The source, not the design: the operator may have edited it, and what
      // they are looking at is what they are deploying.
      const record = await startRun(serveUrl, {
        program: source,
        inputs: design.inputs,
        conversation_id: conversationIdRef.current ?? undefined,
      });
      if (scope !== scopeRef.current) return;
      setSnapshot((current) => (current ? { ...current, team: record.team } : current));
      setRun(record);
      setPhase("observing");
      setTab("events");

      observe(record);
    } catch (cause) {
      if (scope !== scopeRef.current) return;
      setError(cause instanceof Error ? cause.message : String(cause));
      setPhase("review");
    }
  }

  /**
   * Ask the run to stop, and keep saying so until it has.
   *
   * A graceful stop lands at the next tag boundary rather than immediately, so
   * the button has to hold a *stopping* state — one that just went quiet would
   * read as hung for however long the current invocation takes.
   *
   * That state belongs to the run, not to this component. The daemon records it
   * and answers with the record, so the button reads the same field it already
   * watches for every other status — one source, which cannot disagree with
   * itself the way a remembered click and a polled record can.
   */
  async function requestStop() {
    if (!run || isStopping) return;
    const scope = scopeRef.current;
    setError("");
    try {
      const record = await stopRun(serveUrl, run.run_id);
      if (scope === scopeRef.current) setRun(record);
    } catch (cause) {
      if (scope !== scopeRef.current) return;
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  }

  /** Click a component to include it in the next message, click it again to drop it. */
  function toggleComponent(component: string) {
    setSelection((current) =>
      current.includes(component)
        ? current.filter((name) => name !== component)
        : [...current, component],
    );
  }

  function discardDesign() {
    setTab("source");
    setConfirming(false);
    setDesign(null);
    // The diagram those names pointed at is going away with the design.
    setSelection([]);
    // Discarding puts the studio back where it was before the proposal —
    // leaving the topology on screen implies a design is still in play.
    setSnapshot(isDemo ? reviewWorkflow : null);
    setSource(isDemo ? reviewProgram : "");
    setFilename(isDemo ? `${reviewWorkflow.team}.omar` : "program.omar");
    setSourceErrors([]);
    arrangedRef.current = isDemo;
    setPhase("idle");
    setError("");
  }

  // A tag is a time and a microstep, so the time reads as one: `30s:0`, not
  // thirty thousand million and a colon.
  const tag = snapshot?.current_tag
    ? `${formatDuration(snapshot.current_tag.timestamp)}:${snapshot.current_tag.microstep}`
    : "—";
  // How far behind its own schedule the run is. A sequence number counted
  // messages, which told an operator nothing they wanted to know.
  const lag = typeof snapshot?.lag === "number" ? formatDuration(snapshot.lag) : "—";
  const canRun = daemon.state === "live";
  // The compiler answers on every pause in typing. Debounced because a check
  // compiles a real program on disk, and aborted on the next keystroke so a
  // stale answer cannot land after a newer one.
  useEffect(() => {
    const abort = new AbortController();
    const token = ++checkTokenRef.current;
    if (switchingChat) return;
    const current = () => checkTokenRef.current === token;
    const timer = setTimeout(() => {
      // Nothing to check against, or nothing to check: clear rather than leave
      // an error standing for text that is gone.
      if (!canRun || source.trim() === "") {
        setSourceErrors([]);
        return;
      }
      setChecking(true);
      // Checking and projecting are the same question asked twice — is this a
      // program, and what would it do — so they are asked together.
      // The timeline is asked for because the editor draws it. Both answers
      // come off one compile, so they cannot disagree about which text they
      // are describing.
      checkProgram(
        serveUrl,
        source,
        filename,
        {
          timeline: true,
          present: presentKey ? presentKey.split("\u0000") : [],
        },
        abort.signal,
      )
        .then((result) => {
          if (!current()) return;
          setSourceErrors(result.ok ? [] : (result.errors ?? []));
          // The drawing follows the text. Only while the program compiles: a
          // half-typed one has no topology to draw, and blanking the diagram on
          // every transient error would make it flicker for the whole of an
          // edit. And only until a run exists, after which the diagram belongs
          // to what is running rather than to what has since been typed.
          if (result.ok && result.preview && runRef.current === null) {
            setSnapshot(result.preview);
          }
          setSteps(result.steps ?? []);
          setTruncated(result.truncated ?? false);
          // A recomputed projection replaces the tail, so a hand-held position
          // past its end would be pointing at nothing.
          setStepIndex((current) => Math.min(current, Math.max(0, (result.steps?.length ?? 1) - 1)));
        })
        .catch((cause) => {
          if (!current() || abort.signal.aborted) return;
          setSourceErrors([cause instanceof Error ? cause.message : String(cause)]);
        })
        .finally(() => {
          if (current()) setChecking(false);
        });
    }, 400);
    return () => {
      clearTimeout(timer);
      abort.abort();
    };
  }, [source, filename, serveUrl, canRun, presentKey, switchingChat]);

  const isDeployed = run !== null;
  // The daemon's own word for it. Every terminal status replaces it, so the
  // state cannot outlive the run it was asked of.
  const isStopping = run?.status === "stopping";

  // Widths are clamped against the workspace so the diagram always keeps a
  // usable column, whichever divider is being dragged. Below a panel's minimum
  // the drag collapses it rather than leaving a sliver.
  const workspaceWidth = () => workspaceRef.current?.clientWidth ?? 0;
  const resolve = (raw: number, min: number, ceiling: number) =>
    raw < min * COLLAPSE_AT ? 0 : clamp(raw, min, Math.max(min, ceiling));

  const setBuilder = (width: number) => {
    const available = workspaceWidth();
    setBuilderWidth(
      resolve(
        width,
        MIN_BUILDER,
        available ? available - MIN_DIAGRAM - inspectorWidth : width,
      ),
    );
  };
  const setInspector = (width: number) => {
    const available = workspaceWidth();
    setInspectorWidth(
      resolve(
        width,
        MIN_INSPECTOR,
        available ? available - MIN_DIAGRAM - builderWidth : width,
      ),
    );
  };

  // Without a topology there is nothing to divide, so the conversation has the
  // window to itself.
  const columns = snapshot
    ? `${builderWidth}px auto minmax(0, 1fr) auto ${inspectorWidth}px`
    : "minmax(0, 1fr)";

  return (
    <main className="studio-shell">
      <div className="visually-hidden" aria-live="polite">
        <span className={`daemon ${daemon.state}`} aria-label="Runtime mode">
          {isDemo ? "demo topology" : historyUrl}{daemon.state === "offline" ? " · unreachable" : ""}
        </span>
        <span className="connection" data-phase={phase}>{phase}</span>
      </div>

      <div className="studio-content">
        {isDemo ? <aside className="history-rail" aria-label="Omar"><OmarLogo /></aside> : null}
        {!isDemo && (!historyDrawer || drawerOpen) ? (
          <ChatHistory
            serveUrl={historyUrl}
            activeId={selectedId ?? conversationId}
            onSwitchingChange={setSelectingChat}
            mobile={historyDrawer}
            revision={`${historyRevision}:${messages.length}`}
                        collapsed={!historyDrawer && !historyOpen}
            onClose={closeHistory}
                        onOpen={openHistory}
            railButtonRef={historyRailButtonRef}
            onSelect={(conversation) => {
              onSelect(conversation);
              if (historyDrawer) setDrawerOpen(false);
            }}
          />
        ) : null}
      <section
        ref={workspaceRef}
        className="workspace"
        inert={switchingChat}
        aria-busy={switchingChat}
        style={{ gridTemplateColumns: columns }}
      >
        <aside
          className={[
            "builder-panel",
            snapshot && builderWidth === 0 ? "collapsed" : "",
            // Alone in the window, so the thread is read as a column rather
            // than stretched across it.
            snapshot ? "" : "solo",
            !snapshot && messages.length === 0 ? "opening" : "",
          ]
            .filter(Boolean)
            .join(" ")}
        >
          <h1 className="visually-hidden">{conversationTitle}</h1>
          {!isDemo && historyDrawer ? (
            <div className="chat-mobile-controls">
              <button
                type="button"
                ref={historyButtonRef}
                className="history-button"
                onClick={() => setDrawerOpen((open) => !open)}
                aria-expanded={historyVisible}
                aria-controls="chat-history"
                aria-haspopup="dialog"
                aria-label="Open chat history"
              >
                <SidebarIcon direction="open" />
              </button>
            </div>
          ) : null}
          <div className="messages" ref={threadRef}>
            {messages.length === 0 && snapshot ? (
              <p className="builder-status">
                Describe a workflow. The assistant drafts an OMAR program for
                you to confirm before anything runs.
              </p>
            ) : null}
            {messages.map((message) => (
              <ChatMessageView key={message.sequence} message={message} />
            ))}
            {assistantBusy ? <Waiting /> : null}
            {phase === "spawning" ? <Waiting label="Starting the run" /> : null}
          </div>

          {error ? <div className="connection-error">{error}</div> : null}
          {daemon.state === "offline" ? <div className="connection-error" role="status">Cannot reach the runtime at {historyUrl}.</div> : null}

          {selection.length > 0 ? (
            <div className="selection-bar">
              <span className="selection-label">
                [{selection.join(", ")}] selected
              </span>
              <button
                type="button"
                className="selection-clear"
                onClick={() => setSelection([])}
              >
                Clear
              </button>
            </div>
          ) : null}

          <form className="prompt-box" onSubmit={(event) => void submitPrompt(event)}>
            <textarea
              disabled={switchingChat || (!isDemo && !conversationId)}
              value={prompt}
              onChange={(event) => setPrompt(event.target.value)}
              onKeyDown={(event) => {
                if (event.key !== "Enter") return;
                // Shift/Ctrl/Cmd+Enter is a newline; plain Enter sends.
                if (event.shiftKey || event.ctrlKey || event.metaKey) return;
                if (event.nativeEvent.isComposing) return;
                event.preventDefault();
                event.currentTarget.form?.requestSubmit();
              }}
              placeholder="Describe a workflow…  Enter to send, Shift+Enter for a new line"
              aria-label="Describe a workflow"
            />
            <div>
              <div className="composer-tools">
                {phase === "review" && !canRun ? (
                  // Why deploying is unavailable, which is about this composer's
                  // reach rather than about the workflow. The run's own state
                  // left with the buttons: the panel says STATUS beside the
                  // control that changes it, and saying it twice invited the two
                  // to disagree.
                  <span className="composer-status">
                    {daemon.state === "demo"
                      ? "Demo topology — relaunch with OMAR_SERVE_URL to deploy"
                      : `Cannot reach omar serve at ${serveUrl}`}
                  </span>
                ) : (
                  <BackendMenu
                    key={conversationId}
                    serveUrl={hydratedUrl ?? serveUrl}
                    live={daemon.state === "live"}
                  />
                )}
                {!isDemo ? (
                  <button
                    type="button"
                    className="terminal-button"
                    onClick={() => setTerminalAgent(ASSISTANT)}
                    disabled={daemon.state !== "live" || switchingChat || !conversationId}
                    aria-haspopup="dialog"
                  >
                    Inspect on terminal
                  </button>
                ) : null}
              </div>
              {/* Deploying and stopping live on the workflow panel, beside the
                  topology they act on. What is left here acts on the message
                  being written. */}
              <div className="composer-actions">
                <button
                  className="send-button"
                  type="submit"
                  aria-label="Draft workflow"
                  disabled={(switchingChat || (!isDemo && !conversationId)) || assistantBusy || phase === "spawning"}
                >
                  ↑
                </button>
              </div>
            </div>
          </form>
        </aside>

        {snapshot ? (
          <Resizer
            label="the conversation"
            collapsed={builderWidth === 0}
            toward="right"
            onExpand={() => setBuilder(DEFAULT_BUILDER)}
            onDragStart={() => {
              dragOriginRef.current.builder = builderWidth;
            }}
            onDelta={(dx) => setBuilder(dragOriginRef.current.builder + dx)}
            onStep={(dx) => setBuilder(builderWidth + dx)}
          />
        ) : null}

        {snapshot ? (
        <section className="diagram-panel">
          <div className="diagram-heading">
            <div>
              {/* The panel draws a proposal before a run and the run after it,
                  so it has to say which one is on screen. */}
              <span className="eyebrow">
                {snapshot.status === "ready" ? "PROPOSED TOPOLOGY" : "LIVE TOPOLOGY"}
              </span>
              <h2>{snapshot?.team}</h2>
            </div>
            <div className="run-stats">
              <span><small>STATUS</small>{snapshot?.status}</span>
              <span><small>TAG</small>{tag}</span>
              <span><small>LAG</small>{lag}</span>
            </div>
            {(phase === "review" && design) || (phase === "observing" && run) ? (
            <div className="workflow-actions">
              {phase === "review" && design ? (
                <span role="group" aria-label="Deploy design">
                  {confirming ? (
                    <>
                      <button
                        className="secondary-button"
                        onClick={() => setConfirming(false)}
                        type="button"
                      >
                        Cancel
                      </button>
                      <button
                        className="primary-button"
                        onClick={() => void confirmDesign()}
                        type="button"
                        disabled={!canRun}
                        title="This starts real agents"
                      >
                        Confirm deploy
                      </button>
                    </>
                  ) : (
                    <>
                      <button
                        className="secondary-button"
                        onClick={discardDesign}
                        type="button"
                      >
                        Discard
                      </button>
                      <button
                        className="primary-button"
                        onClick={() => setConfirming(true)}
                        type="button"
                        disabled={!canRun}
                      >
                        Deploy
                      </button>
                    </>
                  )}
                </span>
              ) : null}
              {phase === "observing" && run ? (
                <button
                  className="secondary-button"
                  onClick={() => void requestStop()}
                  type="button"
                  disabled={isStopping}
                  title={
                    isStopping
                      ? "The current tag has to close first"
                      : "Closes the current tag, then persists and tears down"
                  }
                >
                  {isStopping ? "Stopping…" : "Stop"}
                </button>
              ) : null}
            </div>
            ) : null}
          </div>
          <DiagramCanvas
            snapshot={snapshot}
            selection={selection}
            onToggleComponent={toggleComponent}
            // Agents outlive the run that spawned them, so a finished run can
            // still be opened; before a run there is nothing behind the node.
            highlight={timelineOpen ? highlighted : undefined}
            onOpenTerminal={canRun && run ? setTerminalAgent : undefined}
            // A web agent has no pane; double-clicking it opens the panel it is
            // answered through instead, which is the only thing there is.
            onOpenPanel={run ? setPanelAgent : undefined}
          />
          {timelineOpen ? (
            <Timeline
              steps={steps}
              index={stepIndex}
              live={following && phase === "observing"}
              truncated={truncated}
              onScrub={(next) => {
                // Scrubbing takes the strip off the run: the operator is
                // looking at a tag, not at where execution has reached.
                setFollowing(false);
                setStepIndex(next);
              }}
              onClose={() => setTimelineOpen(false)}
            />
          ) : (
            <button
              type="button"
              className="timeline-handle"
              onClick={() => setTimelineOpen(true)}
            >
              ▲ Timeline
            </button>
          )}
        </section>
        ) : null}

        {snapshot ? (
          <Resizer
            label="the source pane"
            collapsed={inspectorWidth === 0}
            toward="left"
            onExpand={() => setInspector(DEFAULT_INSPECTOR)}
            onDragStart={() => {
              dragOriginRef.current.inspector = inspectorWidth;
            }}
            onDelta={(dx) => setInspector(dragOriginRef.current.inspector - dx)}
            onStep={(dx) => setInspector(inspectorWidth - dx)}
          />
        ) : null}

        {snapshot ? (
        <aside className={`inspector-panel${inspectorWidth === 0 ? " collapsed" : ""}`}>
          <div className="tabs" role="tablist">
            <button
              role="tab"
              aria-selected={tab === "source"}
              className={tab === "source" ? "active" : ""}
              onClick={() => setTab("source")}
            >
              Source
            </button>
            {/* Events only exist once something has been deployed. */}
            {isDeployed ? (
              <button
                role="tab"
                aria-selected={tab === "events"}
                className={tab === "events" ? "active" : ""}
                onClick={() => setTab("events")}
              >
                Events
              </button>
            ) : null}
            {run && decisionCapabilities?.configured ? (
              <button
                role="tab"
                aria-selected={tab === "suggestions"}
                className={tab === "suggestions" ? "active" : ""}
                onClick={() => setTab("suggestions")}
              >
                Suggestions
              </button>
            ) : null}
          </div>

          {tab === "source" ? (
            <OmarEditor
              source={source}
              filename={filename}
              status={run ? run.status : "draft"}
              errors={sourceErrors}
              checking={checking}
              onSourceChange={setSource}
              onFilenameChange={setFilename}
            />
          ) : tab === "suggestions" && run && decisionCapabilities?.configured ? (
            <Suggestions serveUrl={serveUrl} runId={run.run_id} capabilities={decisionCapabilities} />
          ) : (
            <div className="event-strip" role="tabpanel">
              {events.length ? (
                events.map((event) => (
                  <div key={`${event.sequence}-${event.kind}`}>
                    <span>#{event.sequence}</span>
                    <b>{event.kind.replaceAll("_", " ")}</b>
                  </div>
                ))
              ) : (
                <p>Waiting for the first runtime event…</p>
              )}
            </div>
          )}
        </aside>
        ) : null}
      </section>
      </div>

      {panelAgent && snapshot ? (
        <PortPanel
          agent={panelAgent}
          snapshot={snapshot}
          pending={pending.filter((item) => item.agent === panelAgent)}
          sending={answering}
          onAnswer={(invocation, values) => void answer(invocation, values)}
          onClose={() => setPanelAgent(null)}
        />
      ) : null}

      {terminalAgent ? (
        <AgentTerminal
          serveUrl={serveUrl}
          agent={terminalAgent}
          onClose={() => setTerminalAgent(null)}
        />
      ) : null}
    </main>
  );
}
