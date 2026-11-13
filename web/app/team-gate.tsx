"use client";

import { useCallback, useEffect, useState, type FormEvent, type ReactNode } from "react";
import { OmarLogo } from "./chat-history";
import { Studio, type TeamContext } from "./studio";
import {
  changePassword,
  fetchGate,
  sessionFromPath,
  sessionUrl,
  signIn,
  signOut,
  type Gate,
  type Me,
} from "./lib/team-client";

/** Said wherever someone signs in, because it is the limit of the privacy here. */
export const PRIVACY_NOTE =
  "Mission Control keeps private workflows out of other members' view, but every member's agents run as one account on this machine and can read its files.";

/**
 * Mission Control as the SPA serves it. From a session's own daemon this is
 * just the studio. From `omar gateway` it is a sign-in, a forced password
 * change for a temporary password, then the sessions this account belongs to,
 * each at `/s/<name>/`.
 */
export function TeamGate({ origin, pathname }: { origin: string; pathname: string }) {
  const [gate, setGate] = useState<Gate | null>(null);
  const [error, setError] = useState("");
  const refresh = useCallback(() => {
    fetchGate(origin)
      .then((gate) => { setGate(gate); setError(""); })
      .catch((cause: unknown) => setError(cause instanceof Error ? cause.message : String(cause)));
  }, [origin]);
  useEffect(refresh, [refresh]);

  if (error) return <Card title="Can't reach OMAR"><p role="alert" className="team-error">{error}</p></Card>;
  if (gate === null) return <main className="studio-shell"><p className="workspace-loading" role="status">Opening OMAR…</p></main>;
  if (gate.kind === "daemon") return <Studio serveUrl={origin} />;
  if (gate.kind === "signed-out") return <SignIn origin={origin} onSignedIn={refresh} />;
  const me = gate.me;
  if (me.must_change) return <PasswordForm origin={origin} user={me.user} temporary onChanged={refresh} />;
  const session = sessionFromPath(pathname);
  const membership = me.sessions.find((candidate) => candidate.name === session);
  if (!session || !membership) return <Workspaces origin={origin} me={me} missing={session} />;
  const team: TeamContext = {
    user: me.user,
    role: membership.role,
    session,
    onSwitch: () => window.location.assign("/"),
    onSignOut: () => void signOut(origin).then(() => window.location.assign("/")),
    onChangePassword: () => window.location.assign("/account"),
  };
  return <Studio serveUrl={sessionUrl(origin, session)} team={team} />;
}

function Card({ title, children }: { title: string; children: ReactNode }) {
  return (
    <main className="team-gate">
      <section className="team-card">
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
      <form className="team-form" onSubmit={(event) => void submit(event)}>
        <label>Account<input name="user" autoComplete="username" autoFocus value={user} onChange={(event) => setUser(event.target.value)} /></label>
        <label>Password<input name="password" type="password" autoComplete="current-password" value={password} onChange={(event) => setPassword(event.target.value)} /></label>
        {error ? <p role="alert" className="team-error">{error}</p> : null}
        <button type="submit" disabled={busy || !user || !password}>Sign in</button>
      </form>
      <p className="team-note">An admin creates accounts with <code>omar user add</code>. {PRIVACY_NOTE}</p>
    </Card>
  );
}

export function PasswordForm({ origin, user, temporary = false, onChanged }: {
  origin: string;
  user: string;
  /** Signed in with a password an admin has seen: nothing opens until it changes. */
  temporary?: boolean;
  onChanged: () => void;
}) {
  const [current, setCurrent] = useState("");
  const [next, setNext] = useState("");
  const [again, setAgain] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  async function submit(event: FormEvent) {
    event.preventDefault();
    if (next !== again) {
      setError("The new passwords don't match.");
      return;
    }
    setBusy(true);
    setError("");
    try {
      await changePassword(origin, current, next);
      onChanged();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(false);
    }
  }
  return (
    <Card title={temporary ? "Choose your password" : "Change password"}>
      <p className="team-note">
        {temporary
          ? `Signed in as ${user} with a temporary password. Choose your own; no one else, admins included, can read it.`
          : `Signed in as ${user}. Other browsers signed in to this account are signed out.`}
      </p>
      <form className="team-form" onSubmit={(event) => void submit(event)}>
        <input type="text" name="user" autoComplete="username" value={user} readOnly hidden />
        <label>{temporary ? "Temporary password" : "Current password"}<input name="current" type="password" autoComplete="current-password" autoFocus value={current} onChange={(event) => setCurrent(event.target.value)} /></label>
        <label>New password<input name="new" type="password" autoComplete="new-password" minLength={12} value={next} onChange={(event) => setNext(event.target.value)} /></label>
        <label>New password again<input name="again" type="password" autoComplete="new-password" minLength={12} value={again} onChange={(event) => setAgain(event.target.value)} /></label>
        <p className="team-note">At least 12 characters.</p>
        {error ? <p role="alert" className="team-error">{error}</p> : null}
        <button type="submit" disabled={busy || !current || !next || !again}>Save password</button>
      </form>
    </Card>
  );
}

function Workspaces({ origin, me, missing }: { origin: string; me: Me; missing: string | null }) {
  const [changing, setChanging] = useState(() => typeof window !== "undefined" && window.location.pathname === "/account");
  if (changing) return <PasswordForm origin={origin} user={me.user} onChanged={() => { setChanging(false); window.history.replaceState(null, "", "/"); }} />;
  return (
    <Card title="Workspaces">
      <p className="team-note">Signed in as <strong>{me.user}</strong>.</p>
      {missing ? <p role="alert" className="team-error">You are not a member of “{missing}”.</p> : null}
      {me.sessions.length === 0 ? (
        <p>No workspace yet. Ask an admin to add you: <code>omar -s SESSION member add {me.user}</code></p>
      ) : (
        <ul className="team-workspaces" aria-label="Workspaces">
          {me.sessions.map((session) => (
            <li key={session.name}>
              <a href={`/s/${encodeURIComponent(session.name)}/`}>
                <strong>{session.name}</strong>
                <small>{session.role === "admin" ? "Admin" : "Member"} · {session.state === "ready" ? "Running" : "Stopped"}</small>
              </a>
            </li>
          ))}
        </ul>
      )}
      <div className="team-actions">
        <button type="button" onClick={() => setChanging(true)}>Change password</button>
        <button type="button" onClick={() => void signOut(origin).then(() => window.location.assign("/"))}>Sign out</button>
      </div>
    </Card>
  );
}
