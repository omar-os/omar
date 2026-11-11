"use client";

import { useEffect, useState } from "react";
import type { Role } from "./lib/protocol";
import { fetchTeam, removeMember, setMember, setTeamKey, type Team } from "./lib/team-client";

/**
 * A session admin's page: who is a member, how many chats and live workflows
 * each has (never what is in them), and the provider keys every member's
 * agents use. Accounts are made on the machine with `omar user add`; here an
 * admin only chooses which of them belong to this session.
 */
export function TeamPage({ serveUrl, user, onClose }: { serveUrl: string; user: string; onClose: () => void }) {
  const [team, setTeam] = useState<Team | null>(null);
  const [error, setError] = useState("");
  const [adding, setAdding] = useState("");
  const [keyVar, setKeyVar] = useState("");
  const [keyValue, setKeyValue] = useState("");

  const [revision, setRevision] = useState(0);
  useEffect(() => {
    let live = true;
    fetchTeam(serveUrl)
      .then((team) => { if (live) { setTeam(team); setError(""); } })
      .catch((cause: unknown) => { if (live) setError(cause instanceof Error ? cause.message : String(cause)); });
    return () => { live = false; };
  }, [serveUrl, revision]);

  async function act(action: () => Promise<void>) {
    try {
      await action();
      setRevision((current) => current + 1);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  }

  const members = team?.members ?? [];
  const candidates = (team?.accounts ?? []).filter((account) => !members.some((member) => member.user === account));
  const keyVars = team?.key_vars ?? [];
  const chosenVar = keyVar || keyVars[0] || "";

  return (
    <div className="backends-page" role="dialog" aria-label="Team" onMouseDown={(event) => { if (event.target === event.currentTarget) onClose(); }}>
      <section className="backends-panel team-panel">
        <header>
          <h2>Team</h2>
          <button type="button" onClick={onClose}>Close</button>
        </header>
        <p className="backends-intro">Counts only: a member&apos;s chats stay theirs unless they share them.</p>
        {error ? <p className="backends-error" role="alert">{error}</p> : null}
        <table className="team-members" aria-label="Members">
          <thead><tr><th>Member</th><th>Role</th><th>Chats</th><th>Live workflows</th><th /></tr></thead>
          <tbody>
            {members.map((member) => (
              <tr key={member.user}>
                <td>{member.user}{member.user === user ? " (you)" : ""}</td>
                <td>
                  <select aria-label={`Role of ${member.user}`} value={member.role} onChange={(event) => void act(() => setMember(serveUrl, member.user, event.target.value as Role))}>
                    <option value="member">Member</option>
                    <option value="admin">Admin</option>
                  </select>
                </td>
                <td>{member.chats}</td>
                <td>{member.live_workflows}</td>
                <td><button type="button" onClick={() => void act(() => removeMember(serveUrl, member.user))}>Remove</button></td>
              </tr>
            ))}
          </tbody>
        </table>
        <form className="team-inline" onSubmit={(event) => { event.preventDefault(); if (adding) void act(() => setMember(serveUrl, adding, "member")).then(() => setAdding("")); }}>
          <select aria-label="Account to add" value={adding} onChange={(event) => setAdding(event.target.value)}>
            <option value="">{candidates.length ? "Add an account…" : "Every account is a member"}</option>
            {candidates.map((account) => <option key={account} value={account}>{account}</option>)}
          </select>
          <button type="submit" disabled={!adding}>Add</button>
        </form>
        <h3>API keys</h3>
        <p className="backends-intro">Agents started from now on get these. Members never see them here, but their agents can read them.</p>
        <ul aria-label="API keys">
          {keyVars.map((variable) => (
            <li key={variable}>
              <span className="backend-name">{variable}<small>{team?.keys?.[variable] ?? "Not set"}</small></span>
              {team?.keys?.[variable] ? <button type="button" onClick={() => void act(() => setTeamKey(serveUrl, variable, ""))}>Clear</button> : null}
            </li>
          ))}
        </ul>
        <form className="team-inline" onSubmit={(event) => { event.preventDefault(); void act(() => setTeamKey(serveUrl, chosenVar, keyValue)).then(() => setKeyValue("")); }}>
          <select aria-label="Key" value={chosenVar} onChange={(event) => setKeyVar(event.target.value)}>
            {keyVars.map((variable) => <option key={variable} value={variable}>{variable}</option>)}
          </select>
          <input type="password" aria-label="Key value" placeholder="Paste a key" autoComplete="off" value={keyValue} onChange={(event) => setKeyValue(event.target.value)} />
          <button type="submit" disabled={!keyValue.trim()}>Save</button>
        </form>
      </section>
    </div>
  );
}
