use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::topology::VmState;

/// Boilerplate the generated crate needs. The wire format is one
/// `name<TAB>value` per line because a sealed crate has no JSON to lean on.
const RUNTIME: &str = r####"
use std::collections::BTreeMap;
use std::io::Read;

fn enc(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}

fn dec(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

type In = BTreeMap<String, String>;
type Out = Vec<(String, String)>;

fn get_int(m: &In, k: &str) -> Result<Option<i64>, String> {
    match m.get(k) {
        None => Ok(None),
        Some(v) => v.parse::<i64>().map(Some).map_err(|e| format!("{k}: {e}")),
    }
}

fn get_float(m: &In, k: &str) -> Result<Option<f64>, String> {
    match m.get(k) {
        None => Ok(None),
        Some(v) => v.parse::<f64>().map(Some).map_err(|e| format!("{k}: {e}")),
    }
}

fn get_bool(m: &In, k: &str) -> Result<Option<bool>, String> {
    match m.get(k) {
        None => Ok(None),
        Some(v) => v.parse::<bool>().map(Some).map_err(|e| format!("{k}: {e}")),
    }
}

fn get_string(m: &In, k: &str) -> Result<Option<String>, String> {
    Ok(m.get(k).cloned())
}

fn need<T>(v: Option<T>, k: &str) -> Result<T, String> {
    v.ok_or_else(|| format!("state '{k}' was not supplied"))
}

fn put_int(w: &mut Out, k: &str, v: Option<i64>) {
    if let Some(v) = v {
        w.push((k.to_string(), v.to_string()));
    }
}

fn put_float(w: &mut Out, k: &str, v: Option<f64>) {
    if let Some(v) = v {
        w.push((k.to_string(), v.to_string()));
    }
}

fn put_bool(w: &mut Out, k: &str, v: Option<bool>) {
    if let Some(v) = v {
        w.push((k.to_string(), v.to_string()));
    }
}

fn put_string(w: &mut Out, k: &str, v: Option<String>) {
    if let Some(v) = v {
        w.push((k.to_string(), enc(&v)));
    }
}

fn main() {
    let mut raw = String::new();
    if std::io::stdin().read_to_string(&mut raw).is_err() {
        eprint!("could not read invocation");
        std::process::exit(1);
    }
    let mut lines = raw.lines();
    let id = lines.next().unwrap_or("").to_string();
    let mut triggers: In = BTreeMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once('\t') {
            triggers.insert(k.to_string(), dec(v));
        }
    }
    match dispatch(&id, &triggers) {
        Ok(writes) => {
            let mut out = String::new();
            for (k, v) in writes {
                out.push_str(&k);
                out.push('\t');
                out.push_str(&v);
                out.push('\n');
            }
            print!("{out}");
        }
        Err(error) => {
            eprint!("{error}");
            std::process::exit(1);
        }
    }
}
"####;

/// A built crate and the reactions it answers.
#[derive(Debug)]
pub struct Reactions {
    binary: PathBuf,
    reactions: BTreeSet<String>,
}

/// A bound argument written as the Rust literal the generated crate declares.
fn literal(value: &Value, ty: &str) -> Result<String> {
    Ok(match ty {
        "int" => value
            .as_i64()
            .with_context(|| format!("expected int, got {value}"))?
            .to_string(),
        "float" => {
            let number = value
                .as_f64()
                .with_context(|| format!("expected float, got {value}"))?;
            format!("{number:?}")
        }
        "bool" => value
            .as_bool()
            .with_context(|| format!("expected bool, got {value}"))?
            .to_string(),
        "string" | "path" | "bytes" => format!(
            "{:?}.to_string()",
            value
                .as_str()
                .with_context(|| format!("expected string, got {value}"))?
        ),
        other => bail!("reactions do not support parameter type '{other}'"),
    })
}

/// Held while one run generates and builds in a directory shared by all runs
/// of the same program.
///
/// Without it two runs can write different sources over one `main.rs`, let
/// cargo build whichever landed last, and each publish that binary under its
/// own source's name — so a name would promise a body it was not built from.
struct BuildLock(PathBuf);

impl BuildLock {
    /// A lock older than this belonged to a run that is gone.
    const STALE: Duration = Duration::from_secs(900);

    fn take(dir: &Path) -> Result<Self> {
        let path = dir.join(".building");
        let waited = Instant::now();
        loop {
            match std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&path)
            {
                Ok(mut file) => {
                    let _ = write!(file, "{}", std::process::id());
                    return Ok(BuildLock(path));
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let age = std::fs::metadata(&path)
                        .and_then(|meta| meta.modified())
                        .ok()
                        .and_then(|at| at.elapsed().ok());
                    if age.is_some_and(|age| age > Self::STALE) {
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }
                    if waited.elapsed() > Self::STALE {
                        bail!(
                            "waited too long for another run to finish building in {}",
                            dir.display()
                        );
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(error) => {
                    return Err(error).with_context(|| format!("failed to lock {}", dir.display()))
                }
            }
        }
    }
}

impl Drop for BuildLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Tells one run's draft binary from another's in the same process.
static DRAFTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How long a killed body is given to finish saying what it was saying.
///
/// Its own descendants can hold the stream open after it is gone, so this is
/// bounded rather than waited out: the text is worth having, not worth hanging
/// for.
const SPEAK_GRACE: Duration = Duration::from_millis(250);

/// What one invocation may say on a stream before it is cut off.
///
/// A reaction answers in `name<TAB>value` lines, so a whole answer is small.
/// Without a cap a body could print for its entire deadline and take the run's
/// memory with it, which is a slow failure disguised as work.
const CAPTURE_LIMIT: usize = 8 * 1024 * 1024;

/// Reads until the end, or until `limit` bytes have been kept.
///
/// Reading continues past the cap and is discarded, so the body is never
/// blocked on a full pipe — it is bounded by its deadline, as everything else
/// about it is.
fn drain(mut stream: impl Read, limit: usize) -> std::io::Result<(Vec<u8>, bool)> {
    let mut kept = Vec::new();
    let mut chunk = [0u8; 8192];
    let mut whole = true;
    loop {
        let read = match stream.read(&mut chunk) {
            Ok(0) => return Ok((kept, whole)),
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if kept.len() + read > limit {
            whole = false;
        }
        if kept.len() < limit {
            let room = limit - kept.len();
            kept.extend_from_slice(&chunk[..read.min(room)]);
        }
    }
}

/// Rust's keywords, which a body cannot use as the name of a local.
///
/// Most could be written `r#type`, but the body names these itself and did not
/// write the escape — and `self`, `Self`, `crate` and `super` cannot be raw at
/// all. Refusing the name says so where the program is read, rather than
/// letting rustc say it about generated code.
const RESERVED: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while", "abstract", "become", "box", "do", "final", "macro",
    "override", "priv", "try", "typeof", "unsized", "virtual", "yield",
];

/// Whether a code reaction can bind a name of this type at all.
pub fn supports_type(ty: &str) -> bool {
    rust_type(ty).is_ok()
}

/// Names the generated crate has already taken in the scope a body runs in:
/// the wire map and the writes, the helpers, and the prelude variants a body
/// matches on. A local of the same name would shadow them under the body.
const TAKEN: &[&str] = &[
    "t",
    "w",
    "enc",
    "dec",
    "need",
    "get_int",
    "get_float",
    "get_bool",
    "get_string",
    "put_int",
    "put_float",
    "put_bool",
    "put_string",
    "dispatch",
    "main",
    "In",
    "Out",
    "Some",
    "None",
    "Ok",
    "Err",
];

/// Why a code reaction cannot bind this name, or `None` when it can.
pub fn reserved_name(qualified: &str) -> Option<&'static str> {
    let local = local_name(qualified);
    if RESERVED.contains(&local) {
        Some("a Rust keyword")
    } else if TAKEN.contains(&local) {
        Some("a name the generated crate uses")
    } else {
        None
    }
}

fn rust_type(ty: &str) -> Result<(&'static str, &'static str, &'static str)> {
    // (rust type, getter, putter)
    Ok(match ty {
        "int" => ("i64", "get_int", "put_int"),
        "float" => ("f64", "get_float", "put_float"),
        "bool" => ("bool", "get_bool", "put_bool"),
        "string" | "path" | "bytes" => ("String", "get_string", "put_string"),
        other => bail!("reactions do not support port type '{other}'"),
    })
}

/// The name a body uses for a port, which is the port's own name without the
/// instance path the VM's flat namespace prepends.
fn local_name(qualified: &str) -> &str {
    qualified.rsplit('.').next().unwrap_or(qualified)
}

fn generate(state: &VmState) -> Result<(String, BTreeSet<String>)> {
    let mut source = String::from("// Generated by OMAR. Do not edit.\n");
    source.push_str(RUNTIME);

    let coded: Vec<_> = state
        .reactions
        .iter()
        .filter(|(_, reaction)| reaction.body.is_some())
        .collect();
    let mut ids = BTreeSet::new();
    let mut arms = String::new();

    for (index, (id, reaction)) in coded.iter().enumerate() {
        ids.insert((*id).clone());
        // Its instance's state is `self`, the way a reactor's is in reactor-rs.
        let vars: Vec<_> = state
            .state_vars
            .iter()
            .filter(|(_, var)| var.instance == reaction.instance)
            .collect();
        source.push_str(&format!("\n#[allow(dead_code)]\nstruct S{index} {{\n"));
        for (name, var) in &vars {
            let (rust, _, _) = rust_type(&var.ty)?;
            source.push_str(&format!("    {}: {rust},\n", local_name(name)));
        }
        source.push_str(&format!(
            "}}\n\nimpl S{index} {{\n\
             #[allow(unused_variables, unused_mut, unused_assignments)]\n\
             fn run(&mut self, t: &In) -> Result<Out, String> {{\n"
        ));

        let mut seen = BTreeSet::new();

        // A parameter is a constant, so it binds to its literal rather than to
        // anything off the wire, and it binds immutably: an argument is the
        // instantiation's to choose and not the reaction's to change.
        for (name, param) in state
            .params
            .iter()
            .filter(|(_, param)| param.instance == reaction.instance)
        {
            let (rust, _, _) = rust_type(&param.ty)?;
            let local = local_name(name);
            if !seen.insert(local.to_string()) {
                bail!(
                    "reaction '{id}' has two names spelled '{local}'; \
                     a code body cannot tell them apart"
                );
            }
            source.push_str(&format!(
                "    let {local}: {rust} = {};\n",
                literal(&param.value, &param.ty)?
            ));
        }

        // Triggers bind as Option because a reaction fires when any one of
        // them is present, which is what absence means in the language.
        for trigger in &reaction.triggers {
            let ty = match state.ports.get(trigger) {
                Some(port) => port.ty.clone(),
                // A timer carries the timestamp it fired at.
                None if state.timers.contains_key(trigger) => "int".to_string(),
                None => bail!("reaction '{id}' has unknown trigger '{trigger}'"),
            };
            let (rust, getter, _) = rust_type(&ty)?;
            let local = local_name(trigger);
            if !seen.insert(local.to_string()) {
                bail!(
                    "reaction '{id}' has two triggers named '{local}'; \
                     a code body cannot tell them apart"
                );
            }
            source.push_str(&format!(
                "    let {local}: Option<{rust}> = {getter}(t, \"{trigger}\")?;\n"
            ));
        }

        let mut effects = Vec::new();
        for effect in &reaction.effects {
            let port = state
                .ports
                .get(effect)
                .with_context(|| format!("reaction '{id}' has unknown effect '{effect}'"))?;
            let (rust, _, putter) = rust_type(&port.ty)?;
            let local = local_name(effect);
            if !seen.insert(local.to_string()) {
                bail!(
                    "reaction '{id}' has two ports named '{local}'; \
                     a code body cannot tell them apart"
                );
            }
            source.push_str(&format!("    let mut {local}: Option<{rust}> = None;\n"));
            effects.push((local.to_string(), effect.clone(), putter));
        }

        let body = reaction.body.as_deref().unwrap_or("");
        source.push_str(&format!("    {{\n{body}\n    }}\n"));
        source.push_str("    let mut w: Out = Vec::new();\n");
        for (local, qualified, putter) in effects {
            source.push_str(&format!(
                "    {putter}(&mut w, \"{qualified}\", {local});\n"
            ));
        }
        source.push_str("    Ok(w)\n}\n}\n");

        // The entry point: state in off the wire, the body, state back out.
        source.push_str(&format!(
            "\nfn r{index}(t: &In) -> Result<Out, String> {{\n    let mut s = S{index} {{\n"
        ));
        for (name, var) in &vars {
            let (_, getter, _) = rust_type(&var.ty)?;
            source.push_str(&format!(
                "        {}: need({getter}(t, \"{name}\")?, \"{name}\")?,\n",
                local_name(name)
            ));
        }
        let binding = if vars.is_empty() { "" } else { "mut " };
        source.push_str(&format!("    }};\n    let {binding}w = s.run(t)?;\n"));
        for (name, var) in &vars {
            let (_, _, putter) = rust_type(&var.ty)?;
            source.push_str(&format!(
                "    {putter}(&mut w, \"{name}\", Some(s.{}));\n",
                local_name(name)
            ));
        }
        source.push_str("    Ok(w)\n}\n");

        arms.push_str(&format!("        \"{id}\" => r{index}(t),\n"));
    }

    source.push_str(&format!(
        "\nfn dispatch(id: &str, t: &In) -> Result<Out, String> {{\n\
         \x20   match id {{\n{arms}        \
         other => Err(format!(\"unknown reaction '{{other}}'\")),\n    }}\n}}\n"
    ));
    Ok((source, ids))
}

/// Generate, build, and return a handle, or `None` when the program has no
/// reaction and therefore needs no toolchain at all.
pub fn build(state: &VmState, dir: &Path) -> Result<Option<Reactions>> {
    if state.reactions.values().all(|r| r.body.is_none()) {
        return Ok(None);
    }
    let (source, reactions) = generate(state)?;

    let name = "omar_reactions";
    let suffix = std::env::consts::EXE_SUFFIX;
    let main = dir.join("src").join("main.rs");

    // The generated source names the binary built from it, so a binary is only
    // ever used for the source it was made from. A build that fails publishes
    // nothing and leaves what is already there alone — which matters because
    // the directory belongs to the program, not to one run of it, and another
    // run may be executing that binary right now.
    let mut hasher = DefaultHasher::new();
    source.hash(&mut hasher);
    let published = dir.join(format!("{name}-{:016x}{suffix}", hasher.finish()));

    if !published.exists() {
        std::fs::create_dir_all(dir.join("src"))
            .with_context(|| format!("failed to create {}", dir.display()))?;
        let _lock = BuildLock::take(dir)?;
        // Another run may have published it while this one waited.
        if published.exists() {
            return Ok(Some(Reactions {
                binary: published,
                reactions,
            }));
        }
        // An empty [workspace] keeps the crate standalone wherever it lands.
        std::fs::write(
            dir.join("Cargo.toml"),
            format!(
                "[package]\nname = \"{name}\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n\
                 [dependencies]\n\n[workspace]\n"
            ),
        )?;
        // Leaving an unchanged file untouched is what lets cargo skip the work
        // a new mtime would cost.
        if !matches!(std::fs::read_to_string(&main), Ok(existing) if existing == source) {
            std::fs::write(&main, &source)?;
        }

        let output = Command::new("cargo")
            .arg("build")
            .arg("--release")
            .arg("--offline")
            .current_dir(dir)
            .output()
            .context(
                "failed to invoke cargo; reactions are compiled, so a Rust \
                 toolchain must be installed to run a program that uses them",
            )?;
        if !output.status.success() {
            bail!(
                "failed to compile reactions:\n{}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }

        // Published under its own name only once it exists, so a reader either
        // finds a whole binary or none.
        let built = dir
            .join("target")
            .join("release")
            .join(format!("{name}{suffix}"));
        let draft = dir.join(format!(
            ".{name}.{}.{}{suffix}",
            std::process::id(),
            DRAFTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::copy(&built, &draft)
            .with_context(|| format!("failed to read {}", built.display()))?;
        std::fs::rename(&draft, &published)
            .with_context(|| format!("failed to publish {}", published.display()))?;
    }
    Ok(Some(Reactions {
        binary: published,
        reactions,
    }))
}

/// The type a name carries on the wire, which the VM's tables decide. A timer
/// carries the timestamp it fired at.
fn wire_type<'a>(state: &'a VmState, name: &str) -> Option<&'a str> {
    state
        .ports
        .get(name)
        .map(|port| port.ty.as_str())
        .or_else(|| state.state_vars.get(name).map(|var| var.ty.as_str()))
        .or_else(|| state.timers.contains_key(name).then_some("int"))
}

/// How a value goes onto the wire, which its port's type decides.
fn encode(value: &Value, ty: &str) -> Result<String> {
    Ok(match ty {
        "int" => value
            .as_i64()
            .with_context(|| format!("expected int, got {value}"))?
            .to_string(),
        "float" => value
            .as_f64()
            .with_context(|| format!("expected float, got {value}"))?
            .to_string(),
        "bool" => value
            .as_bool()
            .with_context(|| format!("expected bool, got {value}"))?
            .to_string(),
        _ => value
            .as_str()
            .with_context(|| format!("expected string, got {value}"))?
            .replace('\\', "\\\\")
            .replace('\n', "\\n")
            .replace('\t', "\\t"),
    })
}

fn decode(raw: &str, ty: &str) -> Result<Value> {
    Ok(match ty {
        "int" => json!(raw.parse::<i64>()?),
        "float" => {
            // JSON has no NaN and no infinity, and `json!` turns both into
            // `null` without a word — a port reading nothing where the body
            // wrote something.
            let number = raw.parse::<f64>()?;
            Value::Number(
                serde_json::Number::from_f64(number).with_context(|| {
                    format!("a body wrote '{raw}', which is not a finite number")
                })?,
            )
        }
        "bool" => json!(raw.parse::<bool>()?),
        _ => {
            let mut out = String::new();
            let mut chars = raw.chars();
            while let Some(c) = chars.next() {
                if c != '\\' {
                    out.push(c);
                    continue;
                }
                match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some(other) => out.push(other),
                    None => {}
                }
            }
            Value::String(out)
        }
    })
}

impl Reactions {
    /// Where the built body lives, which a test asserts about.
    #[cfg(test)]
    pub fn binary(&self) -> &Path {
        &self.binary
    }

    pub fn handles(&self, reaction_id: &str) -> bool {
        self.reactions.contains(reaction_id)
    }

    pub fn invoke(
        &self,
        state: &VmState,
        reaction_id: &str,
        triggers: &BTreeMap<String, Value>,
        state_values: &BTreeMap<String, Value>,
        deadline: Duration,
    ) -> Result<Option<BTreeMap<String, Value>>> {
        let mut request = format!("{reaction_id}\n");
        for (name, value) in triggers.iter().chain(state_values) {
            let ty = wire_type(state, name)
                .with_context(|| format!("reaction reads unknown name '{name}'"))?;
            request.push_str(&format!("{name}\t{}\n", encode(value, ty)?));
        }

        let mut child = Command::new(&self.binary)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("failed to run {}", self.binary.display()))?;
        child
            .stdin
            .take()
            .context("reaction subprocess has no stdin")?
            .write_all(request.as_bytes())?;

        // std has no timed wait, so the read runs in a thread and the deadline
        // is enforced on the channel. A body that overruns is killed.
        //
        // Both streams are drained at once. A body that fills one while the
        // other is unread blocks there, and would be reported as slow rather
        // than by whatever it said before it stopped.
        let stdout = child.stdout.take().context("reaction has no stdout")?;
        let stderr = child.stderr.take().context("reaction has no stderr")?;
        // Over a channel rather than a join handle: a body can leave a
        // descendant holding the write end, so the stream may never reach its
        // end even after the body itself is killed. Waiting on it is then a
        // wait with no deadline, which is the one thing an invocation may not
        // do. What it managed to say is worth having, not worth hanging for.
        let (spoke, said) = mpsc::channel();
        std::thread::spawn(move || {
            // Diagnostics, so a cut one is still worth reading.
            let (out, _) = drain(stderr, CAPTURE_LIMIT).unwrap_or_default();
            spoke.send(String::from_utf8_lossy(&out).into_owned()).ok();
        });
        let said = move || said.recv_timeout(SPEAK_GRACE).unwrap_or_default();
        let (done, finished) = mpsc::channel();
        std::thread::spawn(move || {
            done.send(drain(stdout, CAPTURE_LIMIT)).ok();
        });

        let started = Instant::now();
        let out = match finished.recv_timeout(deadline) {
            Ok(out) => {
                // The answer is a protocol frame, so half of one is not a
                // short answer — it is one that cannot be read. A value cut
                // mid-way would otherwise arrive as a shorter value.
                let (out, whole) = out?;
                if !whole {
                    bail!("reaction '{reaction_id}' answered with more than {CAPTURE_LIMIT} bytes");
                }
                out
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                child.kill().ok();
                child.wait().ok();
                said();
                return Ok(None);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                child.kill().ok();
                child.wait().ok();
                said();
                bail!("reaction '{reaction_id}' stopped without answering");
            }
        };
        // Stdout reaching its end is not the body reaching its own: a body can
        // close what it writes to and keep running. The deadline covers this
        // wait too, or `within` would be a promise the body could opt out of.
        let status = loop {
            match child.try_wait()? {
                Some(status) => break status,
                None if started.elapsed() >= deadline => {
                    child.kill().ok();
                    child.wait().ok();
                    said();
                    return Ok(None);
                }
                None => std::thread::sleep(Duration::from_millis(5)),
            }
        };
        // Joined on every path, so a thread is not left per invocation.
        let errors = said();
        if !status.success() {
            bail!("reaction '{reaction_id}' failed: {}", errors.trim());
        }

        let mut writes = BTreeMap::new();
        for line in String::from_utf8_lossy(&out).lines() {
            let Some((name, raw)) = line.split_once('\t') else {
                continue;
            };
            let ty = wire_type(state, name)
                .with_context(|| format!("reaction wrote unknown name '{name}'"))?;
            writes.insert(name.to_string(), decode(raw, ty)?);
        }
        Ok(Some(writes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::topology::{verify, Bytecode};

    /// Long enough that a body which finishes at all finishes inside it, so a
    /// test that is not about the deadline never trips over one.
    const PATIENT: Duration = Duration::from_secs(60);

    /// One node of the ring, with `body` where a prompt would be and no agent,
    /// which is what `omarc` emits for a `reaction`.
    fn node_bytecode(body: &str) -> Bytecode {
        let escaped = serde_json::to_string(body).unwrap();
        serde_json::from_str(&format!(
            r#"{{
              "version": 1,
              "team": "Node",
              "instructions": [
                {{"op":"begin_plan","team":"Node"}},
                {{"op":"define_port","kind":"input","name":"n1.token","type":"int"}},
                {{"op":"define_port","kind":"output","name":"n1.out","type":"int"}},
                {{"op":"define_port","kind":"output","name":"n1.done","type":"int"}},
                {{"op":"install_reaction","id":"n1.reaction.0","agent":"",
                  "triggers":["n1.token"],"effects":["n1.out","n1.done"],
                  "contract":"( n1.out | n1.done )","prompt":"","body":{escaped}}},
                {{"op":"commit_plan"}}
              ]
            }}"#
        ))
        .unwrap()
    }

    const RING_BODY: &str = "if let Some(token) = token {
        if token < 6 { out = Some(token + 1); } else { done = Some(token); }
    }";

    #[test]
    fn a_program_of_prompts_needs_no_toolchain() {
        let mut bytecode = node_bytecode(RING_BODY);
        // Turn the reaction back into a prompt, which is what every existing
        // program is: an agent to ask, and no body.
        bytecode.instructions.insert(
            1,
            serde_json::from_str(r#"{"op":"spawn_agent","name":"n1.agent","backend":"Codex"}"#)
                .unwrap(),
        );
        for instruction in &mut bytecode.instructions {
            if let crate::topology::Instruction::InstallReaction { agent, body, .. } = instruction {
                *agent = "n1.agent".to_string();
                *body = None;
            }
        }
        let state = verify(&bytecode).unwrap();
        let dir = tempfile::tempdir().unwrap();
        assert!(build(&state, dir.path()).unwrap().is_none());
    }

    #[test]
    fn triggers_and_effects_bind_to_their_local_names() {
        let state = verify(&node_bytecode(RING_BODY)).unwrap();
        let (source, ids) = generate(&state).unwrap();
        assert!(ids.contains("n1.reaction.0"));
        assert!(source.contains("let token: Option<i64> = get_int(t, \"n1.token\")?;"));
        assert!(source.contains("let mut out: Option<i64> = None;"));
        assert!(source.contains("put_int(&mut w, \"n1.out\", out);"));
    }

    #[test]
    fn two_ports_sharing_a_local_name_are_refused() {
        let mut bytecode = node_bytecode(RING_BODY);
        bytecode.instructions.insert(
            4,
            serde_json::from_str(
                r#"{"op":"define_port","kind":"input","name":"n2.token","type":"int"}"#,
            )
            .unwrap(),
        );
        for instruction in &mut bytecode.instructions {
            if let crate::topology::Instruction::InstallReaction { triggers, .. } = instruction {
                triggers.push("n2.token".to_string());
            }
        }
        let state = verify(&bytecode).unwrap();
        let error = generate(&state).unwrap_err().to_string();
        assert!(error.contains("two triggers named 'token'"), "{error}");
    }

    /// The issue's sketch cut to two rounds: a counter kept in state.
    fn leader_bytecode() -> Bytecode {
        serde_json::from_str(
            r#"{
              "version": 1,
              "team": "RingLeader",
              "instructions": [
                {"op":"begin_plan","team":"RingLeader"},
                {"op":"declare_instance","name":"leader","parent":"","team":"RingLeader"},
                {"op":"define_port","instance":"leader","kind":"input","name":"leader.token","type":"string"},
                {"op":"define_port","instance":"leader","kind":"output","name":"leader.fwd","type":"string"},
                {"op":"define_port","instance":"leader","kind":"output","name":"leader.done","type":"bool"},
                {"op":"declare_state","instance":"leader","name":"leader.round","type":"int","initial":0},
                {"op":"install_reaction","instance":"leader","id":"leader.reaction.0","agent":"",
                  "triggers":["leader.token"],"effects":["leader.fwd","leader.done"],
                  "contract":"( leader.fwd | leader.done )","prompt":"",
                  "body":"if self.round < 2 { self.round += 1; fwd = token; } else { done = Some(true); }"},
                {"op":"commit_plan"}
              ]
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn state_binds_through_self() {
        let state = verify(&leader_bytecode()).unwrap();
        let (source, _) = generate(&state).unwrap();
        assert!(
            source.contains("struct S0 {\n    round: i64,\n}"),
            "{source}"
        );
        assert!(source.contains("round: need(get_int(t, \"leader.round\")?, \"leader.round\")?,"));
        assert!(source.contains("put_int(&mut w, \"leader.round\", Some(s.round));"));
    }

    #[test]
    #[ignore = "shells out to cargo; run with --ignored"]
    fn state_rides_in_and_out_of_a_body() {
        let dir = tempfile::tempdir().unwrap();
        let state = verify(&leader_bytecode()).unwrap();
        let code = build(&state, dir.path()).unwrap().unwrap();
        let token = BTreeMap::from([("leader.token".to_string(), json!("go"))]);
        let at = |round: i64| BTreeMap::from([("leader.round".to_string(), json!(round))]);

        // Under the limit the body forwards and counts. The count comes back
        // beside the effect; keeping the two apart is the VM's job.
        let writes = code
            .invoke(&state, "leader.reaction.0", &token, &at(0), PATIENT)
            .unwrap()
            .expect("finished well inside its deadline");
        assert_eq!(
            writes,
            BTreeMap::from([
                ("leader.fwd".to_string(), json!("go")),
                ("leader.round".to_string(), json!(1)),
            ])
        );
        let writes = code
            .invoke(&state, "leader.reaction.0", &token, &at(2), PATIENT)
            .unwrap()
            .expect("finished well inside its deadline");
        assert_eq!(
            writes,
            BTreeMap::from([
                ("leader.done".to_string(), json!(true)),
                ("leader.round".to_string(), json!(2)),
            ])
        );

        // State is never absent, so an invocation that brings none is a bug.
        let error = code
            .invoke(
                &state,
                "leader.reaction.0",
                &token,
                &BTreeMap::new(),
                PATIENT,
            )
            .unwrap_err()
            .to_string();
        assert!(error.contains("was not supplied"), "{error}");
    }

    /// The whole path: generate, build with cargo, answer over the wire.
    ///
    /// Ignored by default because it shells out to cargo, which takes long
    /// enough to trip the timing-sensitive tmux and probe tests sharing this
    /// binary. CI runs it as its own step.
    #[test]
    #[ignore = "shells out to cargo; run with --ignored"]
    fn a_compiled_body_answers_its_invocation() {
        let dir = tempfile::tempdir().unwrap();
        let state = verify(&node_bytecode(RING_BODY)).unwrap();
        let code = build(&state, dir.path()).unwrap().expect("a reaction");
        assert!(code.handles("n1.reaction.0"));

        let forwarded = code
            .invoke(
                &state,
                "n1.reaction.0",
                &BTreeMap::from([("n1.token".to_string(), json!(0))]),
                &BTreeMap::new(),
                PATIENT,
            )
            .unwrap()
            .expect("finished well inside its deadline");
        assert_eq!(
            forwarded,
            BTreeMap::from([("n1.out".to_string(), json!(1))])
        );

        let halted = code
            .invoke(
                &state,
                "n1.reaction.0",
                &BTreeMap::from([("n1.token".to_string(), json!(6))]),
                &BTreeMap::new(),
                PATIENT,
            )
            .unwrap()
            .expect("finished well inside its deadline");
        assert_eq!(halted, BTreeMap::from([("n1.done".to_string(), json!(6))]));

        // Absence is free: nothing arrived, so nothing is written, and the
        // optional side of the contract is what makes that legal.
        let quiet = verify(&node_bytecode("let _ = token;")).unwrap();
        let code = build(&quiet, dir.path()).unwrap().unwrap();
        let writes = code
            .invoke(
                &quiet,
                "n1.reaction.0",
                &BTreeMap::new(),
                &BTreeMap::new(),
                PATIENT,
            )
            .unwrap();
        assert!(writes.expect("answered").is_empty());

        // A body that does not typecheck stops the run before it starts.
        let broken = verify(&node_bytecode("out = \"not an int\";")).unwrap();
        let error = build(&broken, dir.path()).unwrap_err().to_string();
        assert!(error.contains("failed to compile reactions"), "{error}");
    }

    /// A body that fails after saying more than a pipe holds.
    ///
    /// Its own account of the failure is what an operator needs, so reading
    /// one stream while the other fills would lose it: the body would block
    /// unread and be reported as slow rather than as broken.
    #[test]
    #[ignore = "shells out to cargo; run with --ignored"]
    fn a_noisy_failure_is_reported_as_a_failure() {
        let dir = tempfile::tempdir().unwrap();
        let state = verify(&node_bytecode(
            r#"for _ in 0..40000 { eprintln!("chatter chatter chatter chatter"); }
               return Err("the body decided to fail".to_string());"#,
        ))
        .unwrap();
        let code = build(&state, dir.path()).unwrap().unwrap();
        let error = code
            .invoke(
                &state,
                "n1.reaction.0",
                &BTreeMap::from([("n1.token".to_string(), json!(0))]),
                &BTreeMap::new(),
                Duration::from_secs(10),
            )
            .unwrap_err()
            .to_string();
        assert!(error.contains("the body decided to fail"), "{error}");
    }
}
