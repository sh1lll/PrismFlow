//! Code sessions: Codex runs in a folder the user picks, driven from
//! PrismFlow's window, and each turn may use a different Codex connection
//! (another tool's "browser sessions", ADR-018). Built on documented CLI behaviour
//! (research/agent-mechanisms-…md): `codex exec --json` for the first turn,
//! `codex exec -p <profile> resume --json <thread>` for the next ones.
//! Turns keep running when the user looks at another page.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::oneshot;

const MAX_EVENTS: usize = 4000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sandbox {
    /// Codex may read the folder and run read-only commands.
    ReadOnly,
    /// Codex may also write inside the folder.
    WorkspaceWrite,
}

impl Sandbox {
    fn arg(self) -> &'static str {
        match self {
            Sandbox::ReadOnly => "read-only",
            Sandbox::WorkspaceWrite => "workspace-write",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnStatus {
    Running,
    Completed,
    Failed,
    Stopped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    pub connection_id: String,
    pub model: String,
    pub prompt: String,
    pub status: TurnStatus,
    pub started_at: u64,
    /// Index of this turn's first event in the session's event list.
    pub first_event: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub folder: PathBuf,
    pub sandbox: Sandbox,
    pub thread_id: Option<String>,
    pub created_at: u64,
    pub turns: Vec<Turn>,
}

impl Session {
    pub fn running(&self) -> bool {
        self.turns.last().is_some_and(|t| t.status == TurnStatus::Running)
    }
}

/// `codex` arguments for a turn (pure, tested).
pub fn turn_args(profile: &str, sandbox: Sandbox, thread: Option<&str>, prompt: &str) -> Vec<String> {
    // A prompt that looks like a flag must not be parsed as one.
    let prompt = if prompt.starts_with('-') { format!(" {prompt}") } else { prompt.to_string() };
    let mut a: Vec<String> = vec!["exec".into(), "-p".into(), profile.into(), "-s".into(), sandbox.arg().into(), "--skip-git-repo-check".into()];
    match thread {
        None => a.extend(["--json".to_string(), prompt]),
        Some(t) => a.extend(["resume".to_string(), "--json".to_string(), t.to_string(), prompt]),
    }
    a
}

struct Entry {
    session: Session,
    events: Vec<Value>,
    stop: Option<oneshot::Sender<()>>,
}

pub struct Sessions {
    file: PathBuf,
    entries: Arc<Mutex<HashMap<String, Entry>>>,
}

#[derive(Debug, Serialize)]
pub struct EventsPage {
    pub session: Session,
    pub events: Vec<Value>,
    pub next: usize,
}

fn now() -> u64 {
    crate::diagnostics::now_secs()
}

impl Sessions {
    pub fn open(state_dir: &Path) -> Self {
        let file = state_dir.join("sessions.json");
        let saved: Vec<Session> = std::fs::read_to_string(&file).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        let entries = saved
            .into_iter()
            .map(|mut s| {
                // A turn that was running when PrismFlow closed did not finish here.
                if let Some(t) = s.turns.last_mut().filter(|t| t.status == TurnStatus::Running) {
                    t.status = TurnStatus::Stopped;
                }
                (s.id.clone(), Entry { session: s, events: Vec::new(), stop: None })
            })
            .collect();
        Self { file, entries: Arc::new(Mutex::new(entries)) }
    }

    fn save(entries: &HashMap<String, Entry>, file: &Path) {
        let mut list: Vec<&Session> = entries.values().map(|e| &e.session).collect();
        list.sort_by_key(|s| std::cmp::Reverse(s.created_at));
        if let Ok(body) = serde_json::to_string_pretty(&list) {
            let _ = crate::config_engine::atomic_write(file, &body);
        }
    }

    pub fn list(&self) -> Vec<Session> {
        let e = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let mut v: Vec<Session> = e.values().map(|x| x.session.clone()).collect();
        v.sort_by_key(|s| std::cmp::Reverse(s.created_at));
        v
    }

    pub fn events(&self, id: &str, since: usize) -> Option<EventsPage> {
        let e = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let x = e.get(id)?;
        let since = since.min(x.events.len());
        Some(EventsPage { session: x.session.clone(), events: x.events[since..].to_vec(), next: x.events.len() })
    }

    pub fn create(&self, folder: &Path, sandbox: Sandbox, title: &str) -> Result<Session, String> {
        if !folder.is_absolute() || !folder.is_dir() {
            return Err(format!("{} is not a folder on this computer.", folder.display()));
        }
        let mut b = [0u8; 8];
        getrandom::fill(&mut b).map_err(|e| e.to_string())?;
        let id: String = b.iter().map(|x| format!("{x:02x}")).collect();
        let s = Session {
            id: id.clone(),
            title: title.trim().chars().take(80).collect::<String>(),
            folder: folder.to_path_buf(),
            sandbox,
            thread_id: None,
            created_at: now(),
            turns: Vec::new(),
        };
        let mut e = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        e.insert(id, Entry { session: s.clone(), events: Vec::new(), stop: None });
        Self::save(&e, &self.file);
        Ok(s)
    }

    /// Start a turn: `codex` from `codex_bin` with `profile`. Returns at once;
    /// events stream into the session as Codex emits them.
    pub fn start_turn(&self, id: &str, codex_bin: &Path, profile: &str, connection_id: &str, model: &str, prompt: &str) -> Result<(), String> {
        let prompt = prompt.trim();
        if prompt.is_empty() {
            return Err("Type a message for Codex.".into());
        }
        let (args, folder) = {
            let mut e = self.entries.lock().unwrap_or_else(|e| e.into_inner());
            let x = e.get_mut(id).ok_or("Unknown session.")?;
            if x.session.running() {
                return Err("Codex is still working on the previous message. Wait, or stop it.".into());
            }
            if !x.session.turns.is_empty() && x.session.thread_id.is_none() {
                return Err("This session can't continue. Start a new one.".into());
            }
            let args = turn_args(profile, x.session.sandbox, x.session.thread_id.as_deref(), prompt);
            x.session.turns.push(Turn {
                connection_id: connection_id.into(),
                model: model.into(),
                prompt: prompt.into(),
                status: TurnStatus::Running,
                started_at: now(),
                first_event: x.events.len(),
            });
            (args, x.session.folder.clone())
        };
        let mut cmd = tokio::process::Command::new(codex_bin);
        cmd.args(&args)
            .current_dir(&folder)
            .env("PATH", crate::adapters::codex::child_path(codex_bin))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                self.finish(id, TurnStatus::Failed, Some(serde_json::json!({"type": "error", "message": format!("could not start Codex: {e}")})));
                return Err(format!("Could not start Codex: {e}"));
            }
        };
        let (tx, mut rx) = oneshot::channel::<()>();
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).get_mut(id).map(|x| x.stop = Some(tx));
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let entries = self.entries.clone();
        let file = self.file.clone();
        let id = id.to_string();
        tokio::spawn(async move {
            let push = |v: Value| {
                let mut e = entries.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(x) = e.get_mut(&id) {
                    if x.session.thread_id.is_none() && v["type"] == "thread.started" {
                        x.session.thread_id = v["thread_id"].as_str().map(String::from);
                    }
                    if x.events.len() < MAX_EVENTS {
                        x.events.push(v);
                    }
                }
            };
            let mut err_tail: Vec<String> = Vec::new();
            let mut stopped = false;
            if let Some(err) = stderr {
                // Drain stderr concurrently so a chatty Codex never blocks.
                let mut lines = BufReader::new(err).lines();
                let (etx, mut erx) = tokio::sync::mpsc::unbounded_channel::<String>();
                tokio::spawn(async move {
                    while let Ok(Some(l)) = lines.next_line().await {
                        let _ = etx.send(l);
                    }
                });
                let mut out_lines = stdout.map(|o| BufReader::new(o).lines());
                loop {
                    tokio::select! {
                        _ = &mut rx => { stopped = true; let _ = child.kill().await; break; }
                        line = async { match out_lines.as_mut() { Some(l) => l.next_line().await, None => Ok(None) } } => {
                            match line {
                                Ok(Some(l)) => if let Ok(v) = serde_json::from_str::<Value>(&l) { push(v) },
                                _ => break,
                            }
                        }
                        Some(l) = erx.recv() => {
                            err_tail.push(crate::diagnostics::redact(&l));
                            if err_tail.len() > 20 { err_tail.remove(0); }
                        }
                    }
                }
            }
            let ok = child.wait().await.is_ok_and(|s| s.success());
            let mut e = entries.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(x) = e.get_mut(&id) {
                let failed_turn = x.events.iter().rev().take_while(|v| v["type"] != "turn.started").any(|v| v["type"] == "turn.failed");
                let status = if stopped { TurnStatus::Stopped } else if ok && !failed_turn { TurnStatus::Completed } else { TurnStatus::Failed };
                if status == TurnStatus::Failed && !err_tail.is_empty() {
                    x.events.push(serde_json::json!({"type": "error", "message": err_tail.join("\n")}));
                }
                if let Some(t) = x.session.turns.last_mut() {
                    t.status = status;
                }
                x.stop = None;
            }
            Self::save(&e, &file);
        });
        Ok(())
    }

    fn finish(&self, id: &str, status: TurnStatus, event: Option<Value>) {
        let mut e = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(x) = e.get_mut(id) {
            if let Some(v) = event {
                x.events.push(v);
            }
            if let Some(t) = x.session.turns.last_mut() {
                t.status = status;
            }
        }
        Self::save(&e, &self.file);
    }

    pub fn stop(&self, id: &str) -> Result<(), String> {
        let mut e = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let x = e.get_mut(id).ok_or("Unknown session.")?;
        match x.stop.take() {
            Some(tx) => {
                let _ = tx.send(());
                Ok(())
            }
            None => Err("Nothing is running in this session.".into()),
        }
    }

    pub fn delete(&self, id: &str) -> Result<(), String> {
        let mut e = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if e.get(id).is_some_and(|x| x.session.running()) {
            return Err("Stop the running turn first.".into());
        }
        e.remove(id).ok_or("Unknown session.")?;
        Self::save(&e, &self.file);
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    #[test]
    fn first_turn_and_resume_args_match_the_verified_cli() {
        assert_eq!(turn_args("prismflow-a", Sandbox::ReadOnly, None, "hi"),
            vec!["exec", "-p", "prismflow-a", "-s", "read-only", "--skip-git-repo-check", "--json", "hi"]);
        assert_eq!(turn_args("prismflow-b", Sandbox::WorkspaceWrite, Some("T1"), "next"),
            vec!["exec", "-p", "prismflow-b", "-s", "workspace-write", "--skip-git-repo-check", "resume", "--json", "T1", "next"]);
        assert_eq!(turn_args("p", Sandbox::ReadOnly, None, "--help").last().unwrap(), " --help");
    }

    /// A fake codex that records its args and streams Codex-shaped events.
    fn fake_codex(dir: &Path) -> PathBuf {
        let p = dir.join("codex");
        std::fs::write(&p, r#"#!/bin/sh
echo "$@" >> "$(dirname "$0")/args.log"
case "$*" in *resume*) ;; *) printf '%s\n' '{"type":"thread.started","thread_id":"T-123"}' ;; esac
printf '%s\n' '{"type":"turn.started"}'
case "$*" in *slow*) sleep 30 ;; esac
printf '%s\n' '{"type":"item.completed","item":{"id":"i1","type":"agent_message","text":"done"}}'
printf '%s\n' '{"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":1}}'
"#).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    async fn wait_idle(s: &Sessions, id: &str) {
        for _ in 0..200 {
            if !s.events(id, 0).unwrap().session.running() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("turn did not finish");
    }

    #[tokio::test]
    async fn turns_stream_events_capture_the_thread_and_switch_profiles() {
        let dir = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let codex = fake_codex(dir.path());
        let s = Sessions::open(dir.path());
        let sess = s.create(work.path(), Sandbox::ReadOnly, "Fix tests").unwrap();
        s.start_turn(&sess.id, &codex, "prismflow-a", "codex_cli:a", "m-a", "first").unwrap();
        wait_idle(&s, &sess.id).await;
        let page = s.events(&sess.id, 0).unwrap();
        assert_eq!(page.session.thread_id.as_deref(), Some("T-123"));
        assert_eq!(page.session.turns[0].status, TurnStatus::Completed);
        assert!(page.events.iter().any(|e| e["item"]["text"] == "done"));
        // Next turn on a different connection resumes the same thread.
        s.start_turn(&sess.id, &codex, "prismflow-b", "codex_cli:b", "m-b", "second").unwrap();
        wait_idle(&s, &sess.id).await;
        let log = std::fs::read_to_string(dir.path().join("args.log")).unwrap();
        assert!(log.lines().nth(1).unwrap().contains("-p prismflow-b") && log.contains("resume --json T-123 second"), "{log}");
        let next = s.events(&sess.id, page.next).unwrap();
        assert!(!next.events.is_empty(), "only new events after `since`");
        // Survives a restart (events are in memory; metadata persists).
        let reopened = Sessions::open(dir.path());
        assert_eq!(reopened.list()[0].turns.len(), 2);
    }

    #[tokio::test]
    async fn stop_kills_a_running_turn() {
        let dir = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let codex = fake_codex(dir.path());
        let s = Sessions::open(dir.path());
        let sess = s.create(work.path(), Sandbox::ReadOnly, "t").unwrap();
        s.start_turn(&sess.id, &codex, "p", "c", "m", "slow please").unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(s.start_turn(&sess.id, &codex, "p", "c", "m", "more").is_err(), "one turn at a time");
        s.stop(&sess.id).unwrap();
        wait_idle(&s, &sess.id).await;
        assert_eq!(s.events(&sess.id, 0).unwrap().session.turns[0].status, TurnStatus::Stopped);
    }

    #[test]
    fn rejects_relative_or_missing_folders() {
        let dir = tempfile::tempdir().unwrap();
        let s = Sessions::open(dir.path());
        assert!(s.create(Path::new("relative/path"), Sandbox::ReadOnly, "x").is_err());
        assert!(s.create(&dir.path().join("missing"), Sandbox::ReadOnly, "x").is_err());
    }
}
