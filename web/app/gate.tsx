"use client";

import { useCallback, useEffect, useState, type FormEvent, type ReactNode } from "react";
import { OmarLogo } from "./chat-history";
import { Studio } from "./studio";
import { fetchGate, sessionFromPath, sessionUrl, signIn, signOut, type Gate as GateState, type GatewaySession } from "./lib/gateway-client";

/**
 * Mission Control as the SPA serves it. From a session's own daemon this is
 * just the studio. From `omar gateway` it signs the operator in, as `root`
 * with the password `omar passwd` set, then lists the sessions here, each at
 * `/s/<name>/`.
 */
export function Gate({ origin, pathname }: { origin: string; pathname: string }) {
  const [gate, setGate] = useState<GateState | null>(null);
  const [error, setError] = useState("");
  const refresh = useCallback(() => {
    fetchGate(origin)
      .then((gate) => { setGate(gate); setError(""); })
      .catch((cause: unknown) => setError(cause instanceof Error ? cause.message : String(cause)));
  }, [origin]);
  useEffect(refresh, [refresh]);

  if (error) return <Card title="Can't reach OMAR"><p role="alert" className="gate-error">{error}</p></Card>;
  if (gate === null) return <main className="studio-shell"><p className="workspace-loading" role="status">Opening OMAR…</p></main>;
  if (gate.kind === "daemon") return <Studio serveUrl={origin} />;
  if (gate.kind === "signed-out") return <SignIn origin={origin} onSignedIn={refresh} />;
  const session = sessionFromPath(pathname);
  const running = gate.sessions.find((candidate) => candidate.name === session && candidate.state === "ready");
  if (!session || !running) return <Sessions origin={origin} user={gate.user} sessions={gate.sessions} missing={session} />;
  const account = {
    user: gate.user,
    session,
    onSwitch: () => window.location.assign("/"),
    onSignOut: () => void signOut(origin).then(() => window.location.assign("/")),
  };
  return <Studio serveUrl={sessionUrl(origin, session)} account={account} />;
}

function Card({ title, children }: { title: string; children: ReactNode }) {
  return (
    <main className="gate">
      <section className="gate-card">
        <div className="brand-lockup"><OmarLogo /><strong>OMAR</strong></div>
        <h1>{title}</h1>
        {children}
      </section>
    </main>
  );
}

function SignIn({ origin, onSignedIn }: { origin: string; onSignedIn: () => void }) {
  const [user, setUser] = useState("");
  const [password, setPassword] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  async function submit(event: FormEvent) {
    event.preventDefault();
    setBusy(true);
    setError("");
    try {
      await signIn(origin, user.trim(), password);
      onSignedIn();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(false);
    }
  }
  return (
    <Card title="Sign in">
      <form className="gate-form" onSubmit={(event) => void submit(event)}>
        <label>Username<input name="username" autoComplete="username" autoCapitalize="none" spellCheck={false} autoFocus value={user} onChange={(event) => setUser(event.target.value)} /></label>
        <label>Password<input name="password" type="password" autoComplete="current-password" value={password} onChange={(event) => setPassword(event.target.value)} /></label>
        {error ? <p role="alert" className="gate-error">{error}</p> : null}
        <button type="submit" disabled={busy || !user || !password}>Sign in</button>
      </form>
      <p className="gate-note"><code>root</code>, with the password set on this machine by <code>omar passwd</code>.</p>
    </Card>
  );
}

function Sessions({ origin, user, sessions, missing }: { origin: string; user: string; sessions: GatewaySession[]; missing: string | null }) {
  return (
    <Card title="Sessions">
      <p className="gate-note">Signed in as <strong>{user}</strong>.</p>
      {missing ? <p role="alert" className="gate-error">“{missing}” is not running. Start it on this machine: <code>omar up -s {missing}</code></p> : null}
      {sessions.length === 0 ? (
        <p>No session yet. Start one on this machine: <code>omar up -s NAME</code></p>
      ) : (
        <ul className="gate-sessions" aria-label="Sessions">
          {sessions.map((session) => (
            <li key={session.name}>
              {session.state === "ready" ? (
                <a href={`/s/${encodeURIComponent(session.name)}/`}><strong>{session.name}</strong><small>Running</small></a>
              ) : (
                <span><strong>{session.name}</strong><small>{session.state === "stopped" ? "Stopped" : session.state}</small></span>
              )}
            </li>
          ))}
        </ul>
      )}
      <div className="gate-actions">
        <button type="button" onClick={() => void signOut(origin).then(() => window.location.assign("/"))}>Sign out</button>
      </div>
    </Card>
  );
}
