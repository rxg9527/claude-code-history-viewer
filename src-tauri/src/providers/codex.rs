use super::ProviderInfo;
use crate::commands::session::{build_matcher, content_matches_scope, SearchScope};
use crate::models::{ClaudeMessage, ClaudeProject, ClaudeSession, TokenUsage};
use crate::utils::{build_provider_message, find_line_ranges, parse_rfc3339_utc};
use chrono::{DateTime, Utc};
use memmap2::Mmap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::UNIX_EPOCH;
use walkdir::WalkDir;

const VSCODE_CONTEXT_PREFIX: &str = "# Context from my IDE setup:";
const CODEX_REQUEST_MARKER: &str = "my request for codex";
const CODEX_PERMISSION_GUARDIAN_NAME: &str = "guardian";
const CODEX_PERMISSION_INSTRUCTIONS_MARKER: &str =
    "You are judging one planned coding-agent action";
const AJK_GIT_COMMIT_SKILL_NAME: &str = "ajk-git-commit";

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CodexSessionFilters {
    /// Master switch. When false or absent, Codex sessions are left untouched.
    #[serde(default)]
    pub enabled: bool,
    /// Whether permissions/guardian approval conversations should be included.
    #[serde(default)]
    pub include_permissions: bool,
    /// Whether ajk-git-commit subagent worker conversations should be included.
    #[serde(default)]
    pub include_git_commit_subagents: bool,
}

lazy_static::lazy_static! {
    static ref SESSION_INDEX_CACHE: Mutex<Option<CodexSessionIndex>> = Mutex::new(None);
}

/// Detect Codex CLI installation
pub fn detect() -> Option<ProviderInfo> {
    let base_path = get_base_path()?;
    let sessions_path = Path::new(&base_path).join("sessions");
    let archived_sessions_path = Path::new(&base_path).join("archived_sessions");

    Some(ProviderInfo {
        id: "codex".to_string(),
        display_name: "Codex CLI".to_string(),
        base_path: base_path.clone(),
        is_available: (sessions_path.exists() && sessions_path.is_dir())
            || (archived_sessions_path.exists() && archived_sessions_path.is_dir()),
    })
}

/// Get the Codex base path
pub fn get_base_path() -> Option<String> {
    // Check $CODEX_HOME first
    if let Ok(codex_home) = std::env::var("CODEX_HOME") {
        let path = PathBuf::from(&codex_home);
        if path.exists() {
            return Some(codex_home);
        }
    }

    // Default: ~/.codex
    let home = dirs::home_dir()?;
    let codex_path = home.join(".codex");
    if codex_path.exists() {
        Some(codex_path.to_string_lossy().to_string())
    } else {
        None
    }
}

fn get_sessions_dir() -> Result<PathBuf, String> {
    let base_path = get_base_path().ok_or_else(|| "Codex not found".to_string())?;
    Ok(Path::new(&base_path).join("sessions"))
}

fn get_archived_sessions_dir() -> Result<PathBuf, String> {
    let base_path = get_base_path().ok_or_else(|| "Codex not found".to_string())?;
    Ok(Path::new(&base_path).join("archived_sessions"))
}

fn get_existing_session_dirs() -> Result<Vec<PathBuf>, String> {
    let sessions_dir = get_sessions_dir()?;
    let archived_sessions_dir = get_archived_sessions_dir()?;

    Ok([sessions_dir, archived_sessions_dir]
        .into_iter()
        .filter(|path| path.exists() && path.is_dir())
        .collect())
}

fn is_rollout_jsonl(path: &Path) -> bool {
    path.file_name()
        .map(|name| name.to_string_lossy().starts_with("rollout-"))
        .unwrap_or(false)
        && path.extension().is_some_and(|ext| ext == "jsonl")
}

fn project_name_from_cwd(cwd: &str) -> String {
    Path::new(cwd)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| cwd.to_string())
}

fn load_thread_titles_from_index(
    index_path: &Path,
) -> Result<HashMap<String, CodexThreadTitle>, String> {
    if !index_path.exists() {
        return Ok(HashMap::new());
    }

    let content = fs::read_to_string(index_path)
        .map_err(|e| format!("Failed to read Codex session index: {e}"))?;
    let mut titles = HashMap::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        let Some(id) = value.get("id").and_then(Value::as_str) else {
            continue;
        };
        let Some(thread_name) = value.get("thread_name").and_then(Value::as_str) else {
            continue;
        };
        let thread_name = thread_name.trim();
        if thread_name.is_empty() {
            continue;
        }

        let updated_at = value
            .get("updated_at")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let should_replace = titles
            .get(id)
            .map(|existing: &CodexThreadTitle| updated_at > existing.updated_at)
            .unwrap_or(true);

        if should_replace {
            titles.insert(
                id.to_string(),
                CodexThreadTitle {
                    name: thread_name.to_string(),
                    updated_at,
                },
            );
        }
    }

    Ok(titles)
}

fn validate_session_path(session_path: &Path, raw_session_path: &str) -> Result<PathBuf, String> {
    let canonical_session = session_path
        .canonicalize()
        .map_err(|e| format!("Failed to resolve session path: {e}"))?;

    let mut canonical_session_dirs = Vec::new();
    for dir in [get_sessions_dir()?, get_archived_sessions_dir()?] {
        if !dir.exists() || !dir.is_dir() {
            continue;
        }
        canonical_session_dirs.push(
            dir.canonicalize()
                .map_err(|e| format!("Failed to resolve Codex session directory: {e}"))?,
        );
    }

    if canonical_session_dirs.is_empty() {
        return Err("No Codex session directories found".to_string());
    }

    let is_allowed = canonical_session_dirs
        .iter()
        .any(|allowed_dir| canonical_session.starts_with(allowed_dir));

    if !is_allowed {
        return Err(format!(
            "Session path is outside Codex session directories: {raw_session_path}"
        ));
    }

    Ok(canonical_session)
}

/// Session metadata extracted from rollout files
#[derive(Clone)]
struct SessionInfo {
    session_id: String,
    cwd: Option<String>,
    #[allow(dead_code)]
    model: Option<String>,
    message_count: usize,
    first_message_time: String,
    last_message_time: String,
    last_modified: String,
    file_path: String,
    has_tool_use: bool,
    summary: Option<String>,
}

#[derive(Debug, Clone)]
struct CodexThreadTitle {
    name: String,
    updated_at: String,
}

#[derive(Clone)]
struct CodexSessionIndex {
    base_path: String,
    thread_index_modified: Option<u128>,
    projects: Vec<ClaudeProject>,
    sessions_by_cwd: HashMap<String, Vec<ClaudeSession>>,
}

pub fn invalidate_session_index_cache() {
    if let Ok(mut cache) = SESSION_INDEX_CACHE.lock() {
        *cache = None;
    }
}

fn get_cached_session_index(base_path: &str) -> Option<CodexSessionIndex> {
    let thread_index_modified = get_thread_index_modified(base_path);
    SESSION_INDEX_CACHE.lock().ok().and_then(|cache| {
        cache
            .as_ref()
            .filter(|index| {
                index.base_path == base_path && index.thread_index_modified == thread_index_modified
            })
            .cloned()
    })
}

fn store_session_index(index: CodexSessionIndex) {
    if let Ok(mut cache) = SESSION_INDEX_CACHE.lock() {
        *cache = Some(index);
    }
}

fn should_apply_codex_filters(filters: Option<&CodexSessionFilters>) -> bool {
    filters.map(|f| f.enabled).unwrap_or(true)
}

fn should_hide_permissions_sessions(filters: Option<&CodexSessionFilters>) -> bool {
    filters
        .map(|f| f.enabled && !f.include_permissions)
        .unwrap_or(true)
}

fn should_hide_git_commit_subagent_sessions(filters: Option<&CodexSessionFilters>) -> bool {
    filters
        .map(|f| f.enabled && !f.include_git_commit_subagents)
        .unwrap_or(true)
}

fn codex_session_allowed(session: &ClaudeSession, filters: Option<&CodexSessionFilters>) -> bool {
    let path = Path::new(&session.file_path);
    if should_hide_permissions_sessions(filters) && is_permissions_approval_session_path(path) {
        return false;
    }
    if should_hide_git_commit_subagent_sessions(filters)
        && is_ajk_git_commit_subagent_session_path(path)
    {
        return false;
    }
    true
}

fn filter_codex_sessions(
    sessions: Vec<ClaudeSession>,
    filters: Option<&CodexSessionFilters>,
) -> Vec<ClaudeSession> {
    if !should_apply_codex_filters(filters) {
        return sessions;
    }
    sessions
        .into_iter()
        .filter(|session| codex_session_allowed(session, filters))
        .collect()
}

fn projects_from_sessions_by_cwd(
    sessions_by_cwd: &HashMap<String, Vec<ClaudeSession>>,
) -> Vec<ClaudeProject> {
    let mut projects: Vec<ClaudeProject> = sessions_by_cwd
        .iter()
        .filter_map(|(cwd, sessions)| {
            if sessions.is_empty() {
                return None;
            }

            let name = project_name_from_cwd(cwd);
            let session_count = sessions.len();
            let message_count: usize = sessions.iter().map(|s| s.message_count).sum();
            let last_modified = sessions
                .iter()
                .map(|s| s.last_modified.as_str())
                .max()
                .unwrap_or("")
                .to_string();

            Some(ClaudeProject {
                name,
                path: format!("codex://{cwd}"),
                actual_path: cwd.clone(),
                session_count,
                message_count,
                last_modified,
                git_info: None,
                provider: Some("codex".to_string()),
                storage_type: None,
                custom_directory_label: None,
            })
        })
        .collect();

    projects.sort_by(|a, b| b.last_modified.cmp(&a.last_modified));
    projects
}

fn filter_codex_projects(
    index: &CodexSessionIndex,
    filters: Option<&CodexSessionFilters>,
) -> Vec<ClaudeProject> {
    if !should_apply_codex_filters(filters) {
        return index.projects.clone();
    }

    let sessions_by_cwd: HashMap<String, Vec<ClaudeSession>> = index
        .sessions_by_cwd
        .iter()
        .filter_map(|(cwd, sessions)| {
            let filtered = filter_codex_sessions(sessions.clone(), filters);
            if filtered.is_empty() {
                None
            } else {
                Some((cwd.clone(), filtered))
            }
        })
        .collect();

    projects_from_sessions_by_cwd(&sessions_by_cwd)
}

fn is_permissions_approval_session_path(path: &Path) -> bool {
    let Ok(file) = File::open(path) else {
        return false;
    };
    let reader = BufReader::new(file);

    for line in reader.lines().map_while(Result::ok).take(20) {
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) != Some("session_meta") {
            continue;
        }

        let Some(payload) = value.get("payload") else {
            return false;
        };

        let is_guardian_subagent = payload
            .pointer("/source/subagent/other")
            .and_then(Value::as_str)
            .map(|name| name == CODEX_PERMISSION_GUARDIAN_NAME)
            .unwrap_or(false);
        let has_permission_instructions = payload
            .pointer("/base_instructions/text")
            .and_then(Value::as_str)
            .map(|text| text.contains(CODEX_PERMISSION_INSTRUCTIONS_MARKER))
            .unwrap_or(false);

        return is_guardian_subagent || has_permission_instructions;
    }

    false
}

fn is_ajk_git_commit_subagent_session_path(path: &Path) -> bool {
    let Ok(file) = File::open(path) else {
        return false;
    };
    let reader = BufReader::new(file);
    let mut is_subagent_worker = false;
    let mut has_git_commit_worker_prompt = false;

    for line in reader.lines().map_while(Result::ok).take(120) {
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };

        if value.get("type").and_then(Value::as_str) == Some("session_meta") {
            if let Some(payload) = value.get("payload") {
                let has_thread_spawn = payload.pointer("/source/subagent/thread_spawn").is_some();
                let thread_source = payload.get("thread_source").and_then(Value::as_str);
                let agent_role = payload.get("agent_role").and_then(Value::as_str);
                is_subagent_worker = has_thread_spawn
                    || (thread_source == Some("subagent") && agent_role == Some("worker"));
            }
            continue;
        }

        let Some(payload) = value.get("payload") else {
            continue;
        };
        if payload.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        if payload.get("role").and_then(Value::as_str) != Some("user") {
            continue;
        }

        if let Some(text) = extract_text_from_content(payload) {
            has_git_commit_worker_prompt = text.contains(AJK_GIT_COMMIT_SKILL_NAME);
            if has_git_commit_worker_prompt {
                break;
            }
        }
    }

    is_subagent_worker && has_git_commit_worker_prompt
}

fn load_thread_titles_from_base_path(
    base_path: &str,
) -> Result<HashMap<String, CodexThreadTitle>, String> {
    load_thread_titles_from_index(&Path::new(base_path).join("session_index.jsonl"))
}

fn get_thread_index_modified(base_path: &str) -> Option<u128> {
    fs::metadata(Path::new(base_path).join("session_index.jsonl"))
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_nanos())
}

fn session_info_to_claude_session(
    info: SessionInfo,
    cwd: &str,
    thread_titles: &HashMap<String, CodexThreadTitle>,
) -> ClaudeSession {
    let thread_title = thread_titles
        .get(&info.session_id)
        .map(|title| title.name.clone());
    let is_renamed = thread_title.is_some();

    ClaudeSession {
        session_id: info.file_path.clone(),
        actual_session_id: info.session_id,
        file_path: info.file_path,
        project_name: project_name_from_cwd(cwd),
        message_count: info.message_count,
        first_message_time: info.first_message_time,
        last_message_time: info.last_message_time,
        last_modified: info.last_modified,
        has_tool_use: info.has_tool_use,
        has_errors: false,
        summary: thread_title.or(info.summary),
        is_renamed,
        provider: Some("codex".to_string()),
        storage_type: None,
        entrypoint: None,
    }
}

fn build_session_index(base_path: &str) -> Result<CodexSessionIndex, String> {
    crate::utils::require_absolute_path(base_path, "Codex base path")?;
    let base = Path::new(base_path);

    let sessions_dir = base.join("sessions");
    let archived_sessions_dir = base.join("archived_sessions");

    let session_dirs: Vec<PathBuf> = [sessions_dir, archived_sessions_dir]
        .into_iter()
        .filter(|path| {
            std::fs::symlink_metadata(path)
                .map(|m| m.file_type().is_dir())
                .unwrap_or(false)
        })
        .collect();

    if session_dirs.is_empty() {
        return Ok(CodexSessionIndex {
            base_path: base_path.to_string(),
            thread_index_modified: get_thread_index_modified(base_path),
            projects: Vec::new(),
            sessions_by_cwd: HashMap::new(),
        });
    }

    let thread_titles = load_thread_titles_from_base_path(base_path).unwrap_or_default();
    let mut sessions_by_cwd: HashMap<String, Vec<ClaudeSession>> = HashMap::new();

    for session_dir in session_dirs {
        for entry in WalkDir::new(session_dir)
            .min_depth(1)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file())
            .filter(|e| is_rollout_jsonl(e.path()))
        {
            let rollout_path = entry.path();

            if let Ok(info) = extract_session_info(rollout_path) {
                let cwd = info.cwd.clone().unwrap_or_else(|| "unknown".to_string());
                let session = session_info_to_claude_session(info, &cwd, &thread_titles);
                sessions_by_cwd.entry(cwd).or_default().push(session);
            }
        }
    }

    for sessions in sessions_by_cwd.values_mut() {
        sessions.sort_by(|a, b| b.last_modified.cmp(&a.last_modified));
    }

    let projects = projects_from_sessions_by_cwd(&sessions_by_cwd);

    Ok(CodexSessionIndex {
        base_path: base_path.to_string(),
        thread_index_modified: get_thread_index_modified(base_path),
        projects,
        sessions_by_cwd,
    })
}

/// Scan Codex projects from a specific base path.
pub fn scan_projects_from_path(base_path: &str) -> Result<Vec<ClaudeProject>, String> {
    scan_projects_from_path_with_filters(base_path, None)
}

pub fn scan_projects_from_path_with_filters(
    base_path: &str,
    filters: Option<&CodexSessionFilters>,
) -> Result<Vec<ClaudeProject>, String> {
    let index = build_session_index(base_path)?;
    let projects = filter_codex_projects(&index, filters);
    store_session_index(index);
    Ok(projects)
}

/// Scan Codex projects from the default location.
pub fn scan_projects() -> Result<Vec<ClaudeProject>, String> {
    scan_projects_with_filters(None)
}

pub fn scan_projects_with_filters(
    filters: Option<&CodexSessionFilters>,
) -> Result<Vec<ClaudeProject>, String> {
    let base = get_base_path().ok_or("Codex base path not found")?;
    scan_projects_from_path_with_filters(&base, filters)
}

/// Load sessions for a Codex project (filtered by cwd)
pub fn load_sessions(
    project_path: &str,
    _exclude_sidechain: bool,
) -> Result<Vec<ClaudeSession>, String> {
    load_sessions_with_filters(project_path, _exclude_sidechain, None)
}

pub fn load_sessions_with_filters(
    project_path: &str,
    _exclude_sidechain: bool,
    filters: Option<&CodexSessionFilters>,
) -> Result<Vec<ClaudeSession>, String> {
    let base_path = get_base_path().ok_or_else(|| "Codex base path not found".to_string())?;

    // Extract cwd from virtual path "codex://{cwd}"
    let target_cwd = project_path
        .strip_prefix("codex://")
        .unwrap_or(project_path);

    if let Some(index) = get_cached_session_index(&base_path) {
        let sessions = index
            .sessions_by_cwd
            .get(target_cwd)
            .cloned()
            .unwrap_or_default();
        return Ok(filter_codex_sessions(sessions, filters));
    }

    let index = build_session_index(&base_path)?;
    let sessions = index
        .sessions_by_cwd
        .get(target_cwd)
        .cloned()
        .unwrap_or_default();
    store_session_index(index);
    Ok(filter_codex_sessions(sessions, filters))
}

/// Load all messages from a Codex rollout file
#[allow(unsafe_code)] // Required for mmap performance optimization
pub fn load_messages(session_path: &str) -> Result<Vec<ClaudeMessage>, String> {
    let path = Path::new(session_path);
    if !path.exists() {
        return Err(format!("Session file not found: {session_path}"));
    }
    let canonical_path = validate_session_path(path, session_path)?;

    let file = File::open(&canonical_path).map_err(|e| e.to_string())?;
    // SAFETY: File is read-only and we only read from the mapping
    let mmap = unsafe { Mmap::map(&file) }.map_err(|e| e.to_string())?;
    let ranges = find_line_ranges(&mmap);

    let mut messages = Vec::new();
    let mut session_id = String::new();
    let mut project_name: Option<String> = None;
    let mut current_model: Option<String> = None;
    let mut prev_input_tokens: u32 = 0;
    let mut prev_output_tokens: u32 = 0;
    let mut prev_cached_tokens: u32 = 0;
    let mut msg_counter = 0u64;

    for &(start, end) in &ranges {
        let line = &mmap[start..end];
        let mut buf = line.to_vec();
        let val: Value = match simd_json::from_slice(&mut buf) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let line_timestamp = val
            .get("timestamp")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let line_type = val.get("type").and_then(|t| t.as_str()).unwrap_or("");

        match line_type {
            "session_meta" => {
                if let Some(payload) = val.get("payload") {
                    session_id = payload
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    project_name = payload
                        .get("cwd")
                        .and_then(|v| v.as_str())
                        .map(project_name_from_cwd);
                }
            }
            "turn_context" => {
                if let Some(payload) = val.get("payload") {
                    if let Some(m) = payload.get("model").and_then(|v| v.as_str()) {
                        current_model = Some(m.to_string());
                    }
                }
            }
            "response_item" => {
                if let Some(payload) = val.get("payload") {
                    if let Some(msg) = convert_codex_item(
                        payload,
                        &session_id,
                        current_model.as_ref(),
                        &line_timestamp,
                        &mut msg_counter,
                    ) {
                        if try_merge_tool_result_into_previous(&mut messages, &msg) {
                            continue;
                        }
                        messages.push(msg);
                    }
                }
            }
            "event_msg" => {
                if let Some(payload) = val.get("payload") {
                    let event_type = payload.get("type").and_then(|t| t.as_str()).unwrap_or("");

                    // Skip events that duplicate response_item messages.
                    // Codex logs user/assistant text in both response_item (type=message)
                    // and event_msg (type=user_message / agent_message) — only keep
                    // the response_item version to avoid showing every message twice.
                    if event_type == "user_message" || event_type == "agent_message" {
                        continue;
                    }

                    if event_type == "token_count" {
                        let usage_totals = extract_token_totals(payload)
                            .or_else(|| extract_last_token_usage(payload));
                        let Some((input, output, cached)) = usage_totals else {
                            continue;
                        };

                        let (delta_input, delta_output, delta_cached) =
                            if prev_input_tokens == 0 && prev_output_tokens == 0 {
                                (input, output, cached)
                            } else {
                                (
                                    input.saturating_sub(prev_input_tokens),
                                    output.saturating_sub(prev_output_tokens),
                                    cached.saturating_sub(prev_cached_tokens),
                                )
                            };
                        prev_input_tokens = input;
                        prev_output_tokens = output;
                        prev_cached_tokens = cached;

                        // Separate non-cached input from cached input for correct billing.
                        // OpenAI's input_tokens includes cached_input_tokens as a subset,
                        // but they are billed at different rates (cached gets 90% discount).
                        let non_cached_input = delta_input.saturating_sub(delta_cached);

                        // Apply to last assistant message without usage
                        if let Some(last_msg) = messages.last_mut() {
                            if last_msg.message_type == "assistant" && last_msg.usage.is_none() {
                                last_msg.usage = Some(TokenUsage {
                                    input_tokens: Some(non_cached_input),
                                    output_tokens: Some(delta_output),
                                    cache_creation_input_tokens: None,
                                    cache_read_input_tokens: Some(delta_cached),
                                    service_tier: None,
                                });
                            }
                        }
                    } else if let Some(msg) =
                        convert_codex_event(payload, &session_id, &line_timestamp, &mut msg_counter)
                    {
                        messages.push(msg);
                    }
                }
            }
            "compacted" => {
                if let Some(payload) = val.get("payload") {
                    let msg = convert_codex_compacted(
                        payload,
                        &session_id,
                        &line_timestamp,
                        &mut msg_counter,
                    );
                    messages.push(msg);
                }
            }
            _ => {}
        }
    }

    if let Some(project_name) = project_name {
        for message in &mut messages {
            message.project_name = Some(project_name.clone());
        }
    }

    Ok(messages)
}

/// Search Codex sessions for a query string
pub(crate) fn search(
    query: &str,
    limit: usize,
    search_scope: SearchScope,
) -> Result<Vec<ClaudeMessage>, String> {
    search_with_filters(query, limit, search_scope, None)
}

pub(crate) fn search_with_filters(
    query: &str,
    limit: usize,
    search_scope: SearchScope,
    filters: Option<&CodexSessionFilters>,
) -> Result<Vec<ClaudeMessage>, String> {
    let session_dirs = get_existing_session_dirs()?;

    if session_dirs.is_empty() {
        return Ok(vec![]);
    }

    let matcher = build_matcher(query);
    let mut results = Vec::new();

    for session_dir in session_dirs {
        for entry in WalkDir::new(session_dir)
            .min_depth(1)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file())
            .filter(|e| is_rollout_jsonl(e.path()))
        {
            let rollout_path = entry.path();
            if should_hide_permissions_sessions(filters)
                && is_permissions_approval_session_path(rollout_path)
            {
                continue;
            }
            if should_hide_git_commit_subagent_sessions(filters)
                && is_ajk_git_commit_subagent_session_path(rollout_path)
            {
                continue;
            }

            if let Ok(messages) = load_messages(&rollout_path.to_string_lossy()) {
                for msg in messages {
                    if let Some(content) = &msg.content {
                        if content_matches_scope(content, &matcher, search_scope) {
                            results.push(msg);
                        }
                    }
                }
            }
        }
    }

    results.sort_by(|a, b| {
        match (
            parse_rfc3339_utc(&a.timestamp),
            parse_rfc3339_utc(&b.timestamp),
        ) {
            (Some(a_ts), Some(b_ts)) => b_ts.cmp(&a_ts),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => b.timestamp.cmp(&a.timestamp),
        }
    });
    results.truncate(limit);

    Ok(results)
}

// ============================================================================
// Internal helpers
// ============================================================================

#[allow(unsafe_code)] // Required for mmap performance optimization
fn extract_session_info(rollout_path: &Path) -> Result<SessionInfo, String> {
    let file = File::open(rollout_path).map_err(|e| e.to_string())?;
    // SAFETY: File is read-only and we only read from the mapping
    let mmap = unsafe { Mmap::map(&file) }.map_err(|e| e.to_string())?;
    let ranges = find_line_ranges(&mmap);

    let mut session_id = String::new();
    let mut cwd = None;
    let mut model = None;
    let mut message_count = 0usize;
    let mut first_time = String::new();
    let mut last_time = String::new();
    let mut has_tool_use = false;
    let mut summary = None;

    for &(start, end) in &ranges {
        let line = &mmap[start..end];
        let mut buf = line.to_vec();
        let val: Value = match simd_json::from_slice(&mut buf) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let line_type = val.get("type").and_then(|t| t.as_str()).unwrap_or("");

        match line_type {
            "session_meta" => {
                if let Some(payload) = val.get("payload") {
                    session_id = payload
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    cwd = payload
                        .get("cwd")
                        .and_then(|v| v.as_str())
                        .map(String::from);
                }
            }
            "turn_context" if model.is_none() => {
                if let Some(payload) = val.get("payload") {
                    model = payload
                        .get("model")
                        .and_then(|v| v.as_str())
                        .map(String::from);
                }
            }
            "response_item" => {
                if let Some(payload) = val.get("payload") {
                    let item_type = payload.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    if item_type == "message" {
                        message_count += 1;

                        let ts = payload
                            .get("created_at")
                            .or_else(|| val.get("timestamp"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();

                        if first_time.is_empty() && !ts.is_empty() {
                            first_time.clone_from(&ts);
                        }
                        if !ts.is_empty() {
                            last_time.clone_from(&ts);
                        }

                        // Extract first user message as summary, skipping
                        // auto-injected wrapper blocks (e.g. <environment_context>)
                        // that codex CLI / Codex Desktop prepend to every session —
                        // they are system context, not a real user prompt.
                        if summary.is_none() {
                            if let Some(role) = payload.get("role").and_then(|r| r.as_str()) {
                                if role == "user" {
                                    if let Some(text) = extract_text_from_content(payload) {
                                        if let Some(title) =
                                            title_candidate_from_user_message(&text)
                                        {
                                            summary = Some(truncate_summary_text(&title));
                                        }
                                    }
                                }
                            }
                        }
                    } else if item_type == "local_shell_call"
                        || item_type == "function_call"
                        || item_type == "custom_tool_call"
                        || item_type == "web_search_call"
                    {
                        has_tool_use = true;
                        message_count += 1;
                    } else if item_type == "function_call_output"
                        || item_type == "custom_tool_call_output"
                    {
                        message_count += 1;
                    }
                }
            }
            _ => {}
        }
    }

    let last_modified = if last_time.is_empty() {
        fs::metadata(rollout_path)
            .ok()
            .and_then(|m| m.modified().ok())
            .map(|t| {
                let dt: DateTime<Utc> = t.into();
                dt.to_rfc3339()
            })
            .unwrap_or_else(|| Utc::now().to_rfc3339())
    } else {
        last_time.clone()
    };

    Ok(SessionInfo {
        session_id,
        cwd,
        model,
        message_count,
        first_message_time: first_time,
        last_message_time: last_time,
        last_modified,
        file_path: rollout_path.to_string_lossy().to_string(),
        has_tool_use,
        summary,
    })
}

fn extract_text_from_content(item: &Value) -> Option<String> {
    let content = item.get("content")?.as_array()?;
    for c in content {
        let ctype = c.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if ctype == "input_text" || ctype == "output_text" || ctype == "text" {
            if let Some(text) = c.get("text").and_then(|t| t.as_str()) {
                return Some(text.to_string());
            }
        }
    }
    None
}

fn truncate_summary_text(text: &str) -> String {
    match text.char_indices().nth(200) {
        Some((idx, _)) => format!("{}...", &text[..idx]),
        None => text.to_string(),
    }
}

fn title_candidate_from_user_message(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty()
        || trimmed.starts_with("# AGENTS.md")
        || trimmed.starts_with("<environment_context>")
    {
        return None;
    }

    if trimmed.starts_with(VSCODE_CONTEXT_PREFIX) {
        return extract_codex_prompt_from_ide_context(trimmed);
    }

    Some(trimmed.to_string())
}

fn extract_codex_prompt_from_ide_context(text: &str) -> Option<String> {
    let normalized = text.replace("\r\n", "\n");
    let lines = normalized.lines().collect::<Vec<_>>();

    // VS Code injects the real prompt as the last "## My request for Codex:"
    // section. Earlier matches can be headings inside active selections.
    let mut prompt: Option<String> = None;
    for (index, line) in lines.iter().enumerate() {
        let Some(inline_prompt) = codex_request_heading_payload(line) else {
            continue;
        };

        if !inline_prompt.is_empty() {
            prompt = Some(inline_prompt.to_string());
            continue;
        }

        let following_prompt = lines[index + 1..].join("\n").trim().to_string();
        prompt = (!following_prompt.is_empty()).then_some(following_prompt);
    }

    prompt
}

fn codex_request_heading_payload(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if !trimmed.starts_with('#') {
        return None;
    }

    let heading = trimmed.trim_start_matches('#').trim_start();
    let lowered = heading.to_ascii_lowercase();
    if !lowered.starts_with(CODEX_REQUEST_MARKER) {
        return None;
    }

    let suffix = heading[CODEX_REQUEST_MARKER.len()..].trim_start();
    if suffix.is_empty() {
        return Some("");
    }

    let Some(separator) = suffix.chars().next() else {
        return Some("");
    };
    if !matches!(separator, ':' | '：' | '-' | '—') {
        return None;
    }

    Some(
        suffix
            .trim_start_matches(|c: char| c.is_whitespace() || matches!(c, ':' | '：' | '-' | '—'))
            .trim(),
    )
}

fn convert_codex_item(
    item: &Value,
    session_id: &str,
    model: Option<&String>,
    line_timestamp: &str,
    counter: &mut u64,
) -> Option<ClaudeMessage> {
    let item_type = item.get("type").and_then(|t| t.as_str())?;
    *counter += 1;

    let uuid = item
        .get("id")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_else(|| format!("codex-{counter}"));

    let timestamp = item
        .get("created_at")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(line_timestamp)
        .to_string();

    match item_type {
        "message" => {
            let role = item.get("role").and_then(|r| r.as_str()).unwrap_or("user");
            let content = convert_codex_content_array(item.get("content"));

            Some(build_codex_message(
                uuid,
                session_id,
                timestamp,
                if role == "user" { "user" } else { "assistant" },
                Some(role),
                content,
                if role == "assistant" {
                    model.cloned()
                } else {
                    None
                },
            ))
        }
        "local_shell_call" => {
            let command = item
                .get("action")
                .and_then(|a| a.get("command"))
                .cloned()
                .unwrap_or(Value::Null);

            let command_str = if let Some(arr) = command.as_array() {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            } else {
                command.as_str().unwrap_or("").to_string()
            };

            let call_id = item
                .get("call_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let content = serde_json::json!([{
                "type": "tool_use",
                "id": call_id,
                "name": "Bash",
                "input": { "command": command_str }
            }]);

            Some(build_codex_message(
                uuid,
                session_id,
                timestamp,
                "assistant",
                Some("assistant"),
                Some(content),
                model.cloned(),
            ))
        }
        "function_call" => {
            let raw_name = item
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let name = map_codex_tool_name(raw_name);
            let call_id = item
                .get("call_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let arguments = item.get("arguments");
            let mut input = parse_tool_arguments(arguments);
            normalize_tool_input(name, &mut input);

            let content = serde_json::json!([{
                "type": "tool_use",
                "id": call_id,
                "name": name,
                "input": input
            }]);

            Some(build_codex_message(
                uuid,
                session_id,
                timestamp,
                "assistant",
                Some("assistant"),
                Some(content),
                model.cloned(),
            ))
        }
        "function_call_output" => {
            let output = item.get("output").cloned().unwrap_or(Value::Null);
            let output = normalize_tool_output(output);
            let call_id = item
                .get("call_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let content = serde_json::json!([{
                "type": "tool_result",
                "tool_use_id": call_id,
                "content": output
            }]);

            Some(build_codex_message(
                uuid,
                session_id,
                timestamp,
                "user",
                Some("user"),
                Some(content),
                None,
            ))
        }
        "custom_tool_call" => {
            let name = item
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("custom_tool");
            let call_id = item
                .get("call_id")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| uuid.clone());
            let mut input = item.get("input").cloned().unwrap_or(Value::Null);
            normalize_custom_tool_input(name, &mut input);

            let content = serde_json::json!([{
                "type": "tool_use",
                "id": call_id,
                "name": name,
                "input": input
            }]);

            Some(build_codex_message(
                uuid,
                session_id,
                timestamp,
                "assistant",
                Some("assistant"),
                Some(content),
                model.cloned(),
            ))
        }
        "custom_tool_call_output" => {
            let output = item.get("output").cloned().unwrap_or(Value::Null);
            let output = normalize_tool_output(output);
            let call_id = item
                .get("call_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let content = serde_json::json!([{
                "type": "tool_result",
                "tool_use_id": call_id,
                "content": output
            }]);

            Some(build_codex_message(
                uuid,
                session_id,
                timestamp,
                "user",
                Some("user"),
                Some(content),
                None,
            ))
        }
        "web_search_call" => {
            let search_id = item
                .get("call_id")
                .or_else(|| item.get("id"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| uuid.clone());
            let action = item
                .get("action")
                .cloned()
                .unwrap_or_else(|| Value::Object(serde_json::Map::default()));
            let input = normalize_web_search_input(action);

            let content = serde_json::json!([{
                "type": "tool_use",
                "id": search_id,
                "name": "WebSearch",
                "input": input
            }]);

            Some(build_codex_message(
                uuid,
                session_id,
                timestamp,
                "assistant",
                Some("assistant"),
                Some(content),
                model.cloned(),
            ))
        }
        "reasoning" => {
            let thinking_text = item
                .get("summary")
                .and_then(|s| s.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();

            if thinking_text.is_empty() {
                return None;
            }

            let content = serde_json::json!([{
                "type": "thinking",
                "thinking": thinking_text
            }]);

            Some(build_codex_message(
                uuid,
                session_id,
                timestamp,
                "assistant",
                Some("assistant"),
                Some(content),
                model.cloned(),
            ))
        }
        _ => None,
    }
}

fn convert_codex_event(
    payload: &Value,
    session_id: &str,
    line_timestamp: &str,
    counter: &mut u64,
) -> Option<ClaudeMessage> {
    let event_type = payload.get("type").and_then(|t| t.as_str())?;

    match event_type {
        "task_started" => {
            *counter += 1;
            let mut msg = build_codex_message(
                format!("codex-event-{counter}"),
                session_id,
                line_timestamp.to_string(),
                "progress",
                None,
                None,
                None,
            );
            msg.data = Some(serde_json::json!({
                "type": "waiting_for_task",
                "status": "started",
                "taskId": payload.get("turn_id").and_then(Value::as_str).unwrap_or_default(),
                "message": "Task started"
            }));
            msg.tool_use_id = payload
                .get("turn_id")
                .and_then(Value::as_str)
                .map(str::to_string);
            Some(msg)
        }
        "task_complete" => {
            *counter += 1;
            let mut msg = build_codex_message(
                format!("codex-event-{counter}"),
                session_id,
                line_timestamp.to_string(),
                "progress",
                None,
                None,
                None,
            );
            msg.data = Some(serde_json::json!({
                "type": "waiting_for_task",
                "status": "completed",
                "taskId": payload.get("turn_id").and_then(Value::as_str).unwrap_or_default(),
                "message": "Task completed"
            }));
            msg.tool_use_id = payload
                .get("turn_id")
                .and_then(Value::as_str)
                .map(str::to_string);
            Some(msg)
        }
        "context_compacted" => {
            *counter += 1;
            let mut msg = build_codex_message(
                format!("codex-event-{counter}"),
                session_id,
                line_timestamp.to_string(),
                "system",
                None,
                Some(serde_json::json!("Context compacted")),
                None,
            );
            msg.subtype = Some("microcompact_boundary".to_string());
            msg.level = Some("info".to_string());
            msg.microcompact_metadata = Some(serde_json::json!({
                "trigger": "context_compacted"
            }));
            Some(msg)
        }
        "agent_reasoning" => {
            let text = payload.get("text").and_then(Value::as_str)?.trim();
            if text.is_empty() {
                return None;
            }
            *counter += 1;
            let content = serde_json::json!([{
                "type": "thinking",
                "thinking": text
            }]);
            Some(build_codex_message(
                format!("codex-event-{counter}"),
                session_id,
                line_timestamp.to_string(),
                "assistant",
                Some("assistant"),
                Some(content),
                None,
            ))
        }
        "turn_aborted" => {
            *counter += 1;
            let reason = payload
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let turn_id = payload.get("turn_id").and_then(Value::as_str).unwrap_or("");
            let content = serde_json::json!([{
                "type": "text",
                "text": format!("[Turn Aborted] reason: {reason}, turn: {turn_id}")
            }]);
            let mut msg = build_codex_message(
                format!("codex-abort-{counter}"),
                session_id,
                line_timestamp.to_string(),
                "system",
                None,
                Some(content),
                None,
            );
            msg.subtype = Some("turn_aborted".to_string());
            msg.level = Some("warning".to_string());
            Some(msg)
        }
        // Unsupported/duplicated Codex events are intentionally ignored.
        _ => None,
    }
}

fn convert_codex_compacted(
    payload: &Value,
    session_id: &str,
    line_timestamp: &str,
    counter: &mut u64,
) -> ClaudeMessage {
    *counter += 1;
    let replacement_history_count = payload
        .get("replacement_history")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);

    let mut msg = build_codex_message(
        format!("codex-compacted-{counter}"),
        session_id,
        line_timestamp.to_string(),
        "system",
        None,
        Some(serde_json::json!("Conversation compacted")),
        None,
    );
    msg.subtype = Some("compact_boundary".to_string());
    msg.level = Some("info".to_string());
    msg.compact_metadata = Some(serde_json::json!({
        "trigger": "compacted",
        "replacementHistoryCount": replacement_history_count
    }));
    msg
}

fn extract_token_totals(payload: &Value) -> Option<(u32, u32, u32)> {
    // Recent Codex logs store usage in payload.info.total_token_usage.
    let total = payload.get("info")?.get("total_token_usage")?;
    let input = total.get("input_tokens")?.as_u64()? as u32;
    let output = total.get("output_tokens")?.as_u64()? as u32;
    let cached = total
        .get("cached_input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    Some((input, output, cached))
}

fn extract_last_token_usage(payload: &Value) -> Option<(u32, u32, u32)> {
    // Fallback for older/newer variants that only include last token usage.
    let last = payload.get("info")?.get("last_token_usage")?;
    let input = last.get("input_tokens")?.as_u64()? as u32;
    let output = last.get("output_tokens")?.as_u64()? as u32;
    let cached = last
        .get("cached_input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    Some((input, output, cached))
}

fn map_codex_tool_name(name: &str) -> &str {
    match name {
        "exec_command" | "shell" | "write_stdin" => "Bash",
        _ => name,
    }
}

fn parse_tool_arguments(arguments: Option<&Value>) -> Value {
    match arguments {
        Some(Value::String(s)) => {
            serde_json::from_str(s).unwrap_or_else(|_| Value::Object(serde_json::Map::default()))
        }
        Some(v) if v.is_object() || v.is_array() => v.clone(),
        _ => Value::Object(serde_json::Map::default()),
    }
}

fn normalize_tool_input(tool_name: &str, input: &mut Value) {
    if tool_name != "Bash" {
        return;
    }

    let Some(obj) = input.as_object_mut() else {
        return;
    };

    // Codex exec_command uses "cmd"; UI Bash renderer expects "command".
    if !obj.contains_key("command") {
        if let Some(cmd) = obj.get("cmd").cloned() {
            match cmd {
                Value::String(_) => {
                    obj.insert("command".to_string(), cmd);
                }
                Value::Array(arr) => {
                    let joined = arr
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" ");
                    obj.insert("command".to_string(), Value::String(joined));
                }
                _ => {}
            }
        }
    }

    if let Some(Value::Array(arr)) = obj.get("command").cloned() {
        let joined = arr
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" ");
        obj.insert("command".to_string(), Value::String(joined));
    }
}

fn normalize_custom_tool_input(tool_name: &str, input: &mut Value) {
    if input.is_object() {
        return;
    }

    if tool_name == "apply_patch" {
        let patch = input.as_str().unwrap_or("").to_string();
        *input = serde_json::json!({ "patch": patch });
        return;
    }

    *input = serde_json::json!({ "input": input.clone() });
}

fn normalize_web_search_input(action: Value) -> Value {
    let Some(action_obj) = action.as_object() else {
        return Value::Object(serde_json::Map::default());
    };

    let mut input = serde_json::Map::default();
    if let Some(query) = action_obj.get("query").and_then(Value::as_str) {
        input.insert("query".to_string(), Value::String(query.to_string()));
    } else if let Some(url) = action_obj.get("url").and_then(Value::as_str) {
        input.insert("query".to_string(), Value::String(url.to_string()));
    } else if let Some(pattern) = action_obj.get("pattern").and_then(Value::as_str) {
        input.insert("query".to_string(), Value::String(pattern.to_string()));
    }
    if let Some(queries) = action_obj.get("queries").cloned() {
        input.insert("queries".to_string(), queries);
    }
    if let Some(action_type) = action_obj.get("type").and_then(Value::as_str) {
        input.insert(
            "action_type".to_string(),
            Value::String(action_type.to_string()),
        );
    }

    Value::Object(input)
}

fn normalize_tool_output(output: Value) -> Value {
    let Value::String(raw) = output else {
        return output;
    };

    // exec_command tool output can be a JSON string: {"output":"...", ...}
    if let Ok(parsed) = serde_json::from_str::<Value>(&raw) {
        if let Some(inner_output) = parsed.get("output") {
            return inner_output.clone();
        }
    }

    // Codex function wrapper output usually embeds "Output:\n{actual stdout}".
    if let Some((_, out)) = raw.split_once("\nOutput:\n") {
        return Value::String(out.to_string());
    }

    Value::String(raw)
}

fn try_merge_tool_result_into_previous(
    messages: &mut [ClaudeMessage],
    msg: &ClaudeMessage,
) -> bool {
    if msg.message_type != "user" {
        return false;
    }

    let Some((tool_use_id, tool_result_block)) = extract_tool_result_block(msg) else {
        return false;
    };

    for prev in messages.iter_mut().rev() {
        if prev.message_type != "assistant" {
            continue;
        }
        if has_matching_tool_use(prev, &tool_use_id) {
            append_content_block(prev, tool_result_block);
            return true;
        }
    }

    false
}

fn extract_tool_result_block(msg: &ClaudeMessage) -> Option<(String, Value)> {
    let arr = msg.content.as_ref()?.as_array()?;
    let first = arr.first()?;
    if first.get("type").and_then(Value::as_str) != Some("tool_result") {
        return None;
    }
    let tool_use_id = first
        .get("tool_use_id")
        .and_then(Value::as_str)?
        .to_string();
    Some((tool_use_id, first.clone()))
}

fn has_matching_tool_use(msg: &ClaudeMessage, tool_use_id: &str) -> bool {
    let Some(arr) = msg.content.as_ref().and_then(Value::as_array) else {
        return false;
    };
    arr.iter().any(|item| {
        item.get("type").and_then(Value::as_str) == Some("tool_use")
            && item.get("id").and_then(Value::as_str) == Some(tool_use_id)
    })
}

fn append_content_block(msg: &mut ClaudeMessage, block: Value) {
    match &mut msg.content {
        Some(Value::Array(arr)) => arr.push(block),
        _ => msg.content = Some(Value::Array(vec![block])),
    }
}

fn extract_first_tool_use(content: Option<&Value>) -> Option<Value> {
    let arr = content?.as_array()?;
    arr.iter()
        .find(|item| item.get("type").and_then(Value::as_str) == Some("tool_use"))
        .cloned()
}

fn convert_codex_content_array(content: Option<&Value>) -> Option<Value> {
    let arr = content?.as_array()?;

    let items: Vec<Value> = arr
        .iter()
        .filter_map(|item| {
            let ctype = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match ctype {
                "input_text" | "output_text" | "text" => {
                    let text = item.get("text").and_then(|t| t.as_str()).unwrap_or("");
                    Some(serde_json::json!({
                        "type": "text",
                        "text": text
                    }))
                }
                "input_image" => {
                    let image_url = item.get("image_url").and_then(Value::as_str).unwrap_or("");
                    if image_url.is_empty() {
                        return None;
                    }
                    Some(serde_json::json!({
                        "type": "image",
                        "source": {
                            "type": "url",
                            "url": image_url
                        }
                    }))
                }
                "refusal" => {
                    let refusal = item
                        .get("refusal")
                        .and_then(|t| t.as_str())
                        .unwrap_or("Refused");
                    Some(serde_json::json!({
                        "type": "text",
                        "text": format!("[Refusal] {refusal}")
                    }))
                }
                _ => None,
            }
        })
        .collect();

    if items.is_empty() {
        None
    } else {
        Some(Value::Array(items))
    }
}

fn build_codex_message(
    uuid: String,
    session_id: &str,
    timestamp: String,
    message_type: &str,
    role: Option<&str>,
    content: Option<Value>,
    model: Option<String>,
) -> ClaudeMessage {
    let tool_use = if message_type == "assistant" {
        extract_first_tool_use(content.as_ref())
    } else {
        None
    };

    let mut msg = build_provider_message(
        "codex",
        uuid,
        session_id,
        timestamp,
        message_type,
        role,
        content,
        model,
    );
    msg.tool_use = tool_use;
    msg
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use serial_test::serial;
    use std::ffi::OsString;
    use std::fs;
    use tempfile::TempDir;

    struct EnvVarGuard {
        key: &'static str,
        original: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &std::path::Path) -> Self {
            let original = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, original }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(value) = self.original.as_ref() {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    #[test]
    fn map_exec_command_to_bash() {
        assert_eq!(map_codex_tool_name("exec_command"), "Bash");
        assert_eq!(map_codex_tool_name("shell"), "Bash");
        assert_eq!(map_codex_tool_name("write_stdin"), "Bash");
        assert_eq!(map_codex_tool_name("batch_execute"), "batch_execute");
    }

    #[test]
    fn normalize_bash_input_maps_cmd_to_command() {
        let mut input = json!({ "cmd": "pwd && ls -la" });
        normalize_tool_input("Bash", &mut input);
        assert_eq!(
            input.get("command").and_then(Value::as_str),
            Some("pwd && ls -la")
        );
    }

    #[test]
    fn normalize_bash_input_maps_command_array_to_string() {
        let mut input = json!({ "command": ["bash", "-lc", "pwd"] });
        normalize_tool_input("Bash", &mut input);
        assert_eq!(
            input.get("command").and_then(Value::as_str),
            Some("bash -lc pwd")
        );
    }

    #[test]
    fn normalize_tool_output_extracts_wrapped_output() {
        let wrapped = "Chunk ID: abc\nWall time: 0.01 seconds\nOutput:\nhello\nworld";
        let out = normalize_tool_output(Value::String(wrapped.to_string()));
        assert_eq!(out.as_str(), Some("hello\nworld"));
    }

    #[test]
    fn normalize_tool_output_extracts_json_output_field() {
        let out = normalize_tool_output(Value::String(
            r#"{"output":"done","metadata":{"exit_code":0}}"#.to_string(),
        ));
        assert_eq!(out.as_str(), Some("done"));
    }

    #[test]
    fn parse_nested_token_count_totals() {
        let payload = json!({
            "type": "token_count",
            "info": {
                "total_token_usage": {
                    "input_tokens": 120,
                    "output_tokens": 30
                }
            }
        });
        assert_eq!(extract_token_totals(&payload), Some((120, 30, 0)));
    }

    #[test]
    fn normalize_custom_tool_input_wraps_apply_patch_text() {
        let mut input = Value::String("*** Begin Patch".to_string());
        normalize_custom_tool_input("apply_patch", &mut input);
        assert_eq!(
            input.get("patch").and_then(Value::as_str),
            Some("*** Begin Patch")
        );
    }

    #[test]
    fn normalize_web_search_input_extracts_query_and_type() {
        let input = normalize_web_search_input(json!({
            "type": "search",
            "query": "codex parser",
            "queries": ["codex parser", "codex rollout"]
        }));
        assert_eq!(
            input.get("query").and_then(Value::as_str),
            Some("codex parser")
        );
        assert_eq!(
            input.get("action_type").and_then(Value::as_str),
            Some("search")
        );
        assert!(input.get("queries").is_some());
    }

    #[test]
    fn convert_content_array_maps_input_image_to_image() {
        let converted = convert_codex_content_array(Some(&json!([
            {
                "type": "input_image",
                "image_url": "data:image/png;base64,abc"
            }
        ])))
        .expect("content should be converted");

        let arr = converted
            .as_array()
            .expect("converted content should be an array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0].get("type").and_then(Value::as_str), Some("image"));
        assert_eq!(
            arr[0]
                .get("source")
                .and_then(|v| v.get("url"))
                .and_then(Value::as_str),
            Some("data:image/png;base64,abc")
        );
    }

    #[test]
    fn convert_custom_tool_call_to_tool_use() {
        let mut counter = 0u64;
        let msg = convert_codex_item(
            &json!({
                "type": "custom_tool_call",
                "name": "apply_patch",
                "call_id": "call_patch_1",
                "input": "*** Begin Patch"
            }),
            "session-1",
            None,
            "2026-02-19T12:00:00Z",
            &mut counter,
        )
        .expect("custom_tool_call should be converted");

        assert_eq!(msg.message_type, "assistant");
        let arr = msg
            .content
            .as_ref()
            .and_then(Value::as_array)
            .expect("content should be an array");
        assert_eq!(arr[0].get("type").and_then(Value::as_str), Some("tool_use"));
        assert_eq!(
            arr[0].get("name").and_then(Value::as_str),
            Some("apply_patch")
        );
        assert_eq!(
            arr[0]
                .get("input")
                .and_then(|v| v.get("patch"))
                .and_then(Value::as_str),
            Some("*** Begin Patch")
        );
    }

    #[test]
    fn convert_custom_tool_call_output_to_tool_result() {
        let mut counter = 0u64;
        let msg = convert_codex_item(
            &json!({
                "type": "custom_tool_call_output",
                "call_id": "call_patch_1",
                "output": "{\"output\":\"Success. Updated files\",\"metadata\":{\"exit_code\":0}}"
            }),
            "session-1",
            None,
            "2026-02-19T12:00:01Z",
            &mut counter,
        )
        .expect("custom_tool_call_output should be converted");

        assert_eq!(msg.message_type, "user");
        let arr = msg
            .content
            .as_ref()
            .and_then(Value::as_array)
            .expect("content should be an array");
        assert_eq!(
            arr[0].get("type").and_then(Value::as_str),
            Some("tool_result")
        );
        assert_eq!(
            arr[0].get("tool_use_id").and_then(Value::as_str),
            Some("call_patch_1")
        );
        assert_eq!(
            arr[0].get("content").and_then(Value::as_str),
            Some("Success. Updated files")
        );
    }

    #[test]
    fn convert_web_search_call_to_web_search_tool_use() {
        let mut counter = 0u64;
        let msg = convert_codex_item(
            &json!({
                "type": "web_search_call",
                "action": {
                    "type": "open_page",
                    "url": "https://example.com"
                }
            }),
            "session-1",
            None,
            "2026-02-19T12:00:02Z",
            &mut counter,
        )
        .expect("web_search_call should be converted");

        assert_eq!(msg.message_type, "assistant");
        let arr = msg
            .content
            .as_ref()
            .and_then(Value::as_array)
            .expect("content should be an array");
        assert_eq!(arr[0].get("type").and_then(Value::as_str), Some("tool_use"));
        assert_eq!(
            arr[0].get("name").and_then(Value::as_str),
            Some("WebSearch")
        );
        assert_eq!(
            arr[0]
                .get("input")
                .and_then(|v| v.get("query"))
                .and_then(Value::as_str),
            Some("https://example.com")
        );
    }

    #[test]
    fn merge_tool_result_into_previous_tool_use_message() {
        let mut messages = vec![build_codex_message(
            "assistant-1".to_string(),
            "session-1",
            "2026-02-19T12:00:00Z".to_string(),
            "assistant",
            Some("assistant"),
            Some(json!([{
                "type": "tool_use",
                "id": "call_abc",
                "name": "Bash",
                "input": { "command": "pwd" }
            }])),
            None,
        )];

        let result_msg = build_codex_message(
            "user-1".to_string(),
            "session-1",
            "2026-02-19T12:00:01Z".to_string(),
            "user",
            Some("user"),
            Some(json!([{
                "type": "tool_result",
                "tool_use_id": "call_abc",
                "content": "ok"
            }])),
            None,
        );

        assert!(try_merge_tool_result_into_previous(
            &mut messages,
            &result_msg
        ));
        let merged_arr = messages[0]
            .content
            .as_ref()
            .and_then(Value::as_array)
            .expect("assistant message content should be an array");
        assert_eq!(merged_arr.len(), 2);
        assert_eq!(
            merged_arr[1].get("type").and_then(Value::as_str),
            Some("tool_result")
        );
    }

    #[test]
    fn build_codex_message_sets_tool_use_from_content() {
        let msg = build_codex_message(
            "assistant-1".to_string(),
            "session-1",
            "2026-02-19T12:00:00Z".to_string(),
            "assistant",
            Some("assistant"),
            Some(json!([{
                "type": "tool_use",
                "id": "call_1",
                "name": "Bash",
                "input": {"command": "pwd"}
            }])),
            None,
        );

        assert!(msg.tool_use.is_some());
        assert_eq!(
            msg.tool_use
                .as_ref()
                .and_then(|v| v.get("name"))
                .and_then(Value::as_str),
            Some("Bash")
        );
    }

    #[test]
    fn convert_task_started_event_to_progress_message() {
        let mut counter = 0u64;
        let msg = convert_codex_event(
            &json!({
                "type": "task_started",
                "turn_id": "turn_1"
            }),
            "session-1",
            "2026-02-19T12:00:00Z",
            &mut counter,
        )
        .expect("task_started should be converted");

        assert_eq!(msg.message_type, "progress");
        assert_eq!(
            msg.data
                .as_ref()
                .and_then(|v| v.get("status"))
                .and_then(Value::as_str),
            Some("started")
        );
    }

    #[test]
    fn convert_context_compacted_event_to_system_message() {
        let mut counter = 0u64;
        let msg = convert_codex_event(
            &json!({
                "type": "context_compacted"
            }),
            "session-1",
            "2026-02-19T12:00:00Z",
            &mut counter,
        )
        .expect("context_compacted should be converted");

        assert_eq!(msg.message_type, "system");
        assert_eq!(msg.subtype.as_deref(), Some("microcompact_boundary"));
    }

    #[test]
    fn convert_agent_reasoning_event_to_thinking_message() {
        let mut counter = 0u64;
        let msg = convert_codex_event(
            &json!({
                "type": "agent_reasoning",
                "text": "**Inspecting parsers**"
            }),
            "session-1",
            "2026-02-19T12:00:00Z",
            &mut counter,
        )
        .expect("agent_reasoning should be converted");

        assert_eq!(msg.message_type, "assistant");
        let arr = msg
            .content
            .as_ref()
            .and_then(Value::as_array)
            .expect("content should be an array");
        assert_eq!(arr[0].get("type").and_then(Value::as_str), Some("thinking"));
        assert_eq!(
            arr[0].get("thinking").and_then(Value::as_str),
            Some("**Inspecting parsers**")
        );
    }

    #[test]
    fn convert_agent_reasoning_event_skips_empty_text() {
        let mut counter = 0u64;
        let msg = convert_codex_event(
            &json!({
                "type": "agent_reasoning",
                "text": "   "
            }),
            "session-1",
            "2026-02-19T12:00:00Z",
            &mut counter,
        );

        assert!(msg.is_none());
        assert_eq!(counter, 0);
    }

    #[test]
    fn convert_agent_message_event_not_handled() {
        // agent_message events are skipped in load_messages() to avoid
        // duplicating response_item messages. convert_codex_event should
        // return None for them.
        let mut counter = 0u64;
        let msg = convert_codex_event(
            &json!({
                "type": "agent_message",
                "message": "Working on requested changes"
            }),
            "session-1",
            "2026-02-19T12:00:00Z",
            &mut counter,
        );
        assert!(msg.is_none());
    }

    #[test]
    fn convert_user_message_event_not_handled() {
        // user_message events are skipped in load_messages() to avoid
        // duplicating response_item messages. convert_codex_event should
        // return None for them.
        let mut counter = 0u64;
        let msg = convert_codex_event(
            &json!({
                "type": "user_message",
                "message": "Please patch this file"
            }),
            "session-1",
            "2026-02-19T12:00:00Z",
            &mut counter,
        );
        assert!(msg.is_none());
    }

    #[test]
    fn convert_compacted_line_to_system_message() {
        let mut counter = 0u64;
        let msg = convert_codex_compacted(
            &json!({
                "message": "",
                "replacement_history": [{"type":"message"}]
            }),
            "session-1",
            "2026-02-19T12:00:00Z",
            &mut counter,
        );

        assert_eq!(msg.message_type, "system");
        assert_eq!(msg.subtype.as_deref(), Some("compact_boundary"));
        assert_eq!(
            msg.compact_metadata
                .as_ref()
                .and_then(|v| v.get("replacementHistoryCount"))
                .and_then(Value::as_u64),
            Some(1)
        );
    }

    #[test]
    #[serial]
    fn load_messages_parses_codex_rollout_end_to_end() {
        let tmp = TempDir::new().expect("temp dir should be created");
        let codex_home = tmp.path().join("codex-home");
        let sessions_dir = codex_home.join("sessions");
        fs::create_dir_all(&sessions_dir).expect("sessions dir should be created");
        let _guard = EnvVarGuard::set("CODEX_HOME", &codex_home);
        let rollout_path = sessions_dir.join("rollout-2026-02-19.jsonl");

        let lines = vec![
            json!({
                "timestamp": "2026-02-19T12:00:00Z",
                "type": "session_meta",
                "payload": { "id": "sess-1" }
            }),
            json!({
                "timestamp": "2026-02-19T12:00:01Z",
                "type": "turn_context",
                "payload": { "model": "gpt-5-codex" }
            }),
            json!({
                "timestamp": "2026-02-19T12:00:02Z",
                "type": "response_item",
                "payload": {
                    "id": "item-1",
                    "type": "function_call",
                    "name": "exec_command",
                    "call_id": "call_1",
                    "arguments": "{\"cmd\":\"pwd\"}"
                }
            }),
            json!({
                "timestamp": "2026-02-19T12:00:03Z",
                "type": "response_item",
                "payload": {
                    "id": "item-2",
                    "type": "function_call_output",
                    "call_id": "call_1",
                    "output": "{\"output\":\"/tmp\",\"metadata\":{\"exit_code\":0}}"
                }
            }),
            json!({
                "timestamp": "2026-02-19T12:00:04Z",
                "type": "response_item",
                "payload": {
                    "id": "item-3",
                    "type": "message",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "done" }]
                }
            }),
            json!({
                "timestamp": "2026-02-19T12:00:05Z",
                "type": "event_msg",
                "payload": {
                    "type": "token_count",
                    "info": {
                        "total_token_usage": {
                            "input_tokens": 100,
                            "output_tokens": 20
                        }
                    }
                }
            }),
            json!({
                "timestamp": "2026-02-19T12:00:06Z",
                "type": "event_msg",
                "payload": {
                    "type": "task_started",
                    "turn_id": "turn_1"
                }
            }),
            json!({
                "timestamp": "2026-02-19T12:00:07Z",
                "type": "event_msg",
                "payload": {
                    "type": "task_complete",
                    "turn_id": "turn_1"
                }
            }),
            json!({
                "timestamp": "2026-02-19T12:00:08Z",
                "type": "event_msg",
                "payload": {
                    "type": "context_compacted"
                }
            }),
            json!({
                "timestamp": "2026-02-19T12:00:09Z",
                "type": "compacted",
                "payload": {
                    "replacement_history": [{ "type": "message" }, { "type": "summary" }]
                }
            }),
        ];

        let content = lines
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&rollout_path, format!("{content}\n")).expect("fixture should be written");

        let messages = load_messages(
            rollout_path
                .to_str()
                .expect("rollout path should be valid UTF-8"),
        )
        .expect("rollout should be parsed");

        assert_eq!(messages.len(), 6);
        assert_eq!(messages[0].message_type, "assistant");
        assert_eq!(messages[1].message_type, "assistant");
        assert_eq!(messages[2].message_type, "progress");
        assert_eq!(messages[3].message_type, "progress");
        assert_eq!(messages[4].message_type, "system");
        assert_eq!(messages[5].message_type, "system");

        let first_blocks = messages[0]
            .content
            .as_ref()
            .and_then(Value::as_array)
            .expect("first message content should be an array");
        assert_eq!(first_blocks.len(), 2);
        assert_eq!(
            first_blocks[0].get("type").and_then(Value::as_str),
            Some("tool_use")
        );
        assert_eq!(
            first_blocks[1].get("type").and_then(Value::as_str),
            Some("tool_result")
        );
        assert_eq!(
            first_blocks[1].get("content").and_then(Value::as_str),
            Some("/tmp")
        );

        assert_eq!(
            messages[0]
                .tool_use
                .as_ref()
                .and_then(|v| v.get("name"))
                .and_then(Value::as_str),
            Some("Bash")
        );
        assert_eq!(messages[0].model.as_deref(), Some("gpt-5-codex"));
        assert_eq!(messages[1].model.as_deref(), Some("gpt-5-codex"));

        assert_eq!(
            messages[1].usage.as_ref().and_then(|u| u.input_tokens),
            Some(100)
        );
        assert_eq!(
            messages[1].usage.as_ref().and_then(|u| u.output_tokens),
            Some(20)
        );

        assert_eq!(
            messages[2]
                .data
                .as_ref()
                .and_then(|v| v.get("status"))
                .and_then(Value::as_str),
            Some("started")
        );
        assert_eq!(
            messages[3]
                .data
                .as_ref()
                .and_then(|v| v.get("status"))
                .and_then(Value::as_str),
            Some("completed")
        );
        assert_eq!(
            messages[4].subtype.as_deref(),
            Some("microcompact_boundary")
        );
        assert_eq!(messages[5].subtype.as_deref(), Some("compact_boundary"));
        assert_eq!(
            messages[5]
                .compact_metadata
                .as_ref()
                .and_then(|v| v.get("replacementHistoryCount"))
                .and_then(Value::as_u64),
            Some(2)
        );

        assert!(messages
            .iter()
            .all(|m| m.provider.as_deref() == Some("codex")));
        assert!(messages.iter().all(|m| m.session_id == "sess-1"));
    }

    #[test]
    #[serial]
    fn search_applies_scope_before_limit() {
        let tmp = TempDir::new().expect("temp dir should be created");
        let codex_home = tmp.path().join("codex-home");
        let sessions_dir = codex_home.join("sessions");
        fs::create_dir_all(&sessions_dir).expect("sessions dir should be created");
        let _guard = EnvVarGuard::set("CODEX_HOME", &codex_home);
        let rollout_path = sessions_dir.join("rollout-search-scope.jsonl");

        let lines = [
            json!({
                "timestamp": "2026-03-01T10:00:00Z",
                "type": "session_meta",
                "payload": { "id": "sess-search-scope" }
            }),
            json!({
                "timestamp": "2026-03-01T10:00:01Z",
                "type": "response_item",
                "payload": {
                    "id": "tool-output",
                    "type": "function_call_output",
                    "call_id": "call-1",
                    "output": "needle from tool result"
                }
            }),
            json!({
                "timestamp": "2026-03-01T10:00:02Z",
                "type": "response_item",
                "payload": {
                    "id": "text-output",
                    "type": "message",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "needle from plain text" }]
                }
            }),
        ];

        let content = lines
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&rollout_path, format!("{content}\n")).expect("fixture should be written");

        let results = search("needle", 1, SearchScope::Text).expect("search should succeed");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].uuid, "text-output");
    }

    #[test]
    #[serial]
    fn load_messages_sets_project_name_from_cwd() {
        let tmp = TempDir::new().expect("temp dir should be created");
        let codex_home = tmp.path().join("codex-home");
        let sessions_dir = codex_home.join("sessions");
        fs::create_dir_all(&sessions_dir).expect("sessions dir should be created");
        let _guard = EnvVarGuard::set("CODEX_HOME", &codex_home);
        let rollout_path = sessions_dir.join("rollout-project-name.jsonl");

        let lines = [
            json!({
                "timestamp": "2026-03-01T10:00:00Z",
                "type": "session_meta",
                "payload": {
                    "id": "sess-project-name",
                    "cwd": "/Users/Ruan/work/claude-code-history-viewer"
                }
            }),
            json!({
                "timestamp": "2026-03-01T10:00:01Z",
                "type": "response_item",
                "payload": {
                    "id": "message-1",
                    "type": "message",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "hello" }]
                }
            }),
        ];

        let content = lines
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&rollout_path, format!("{content}\n")).expect("fixture should be written");

        let messages = load_messages(
            rollout_path
                .to_str()
                .expect("rollout path should be valid UTF-8"),
        )
        .expect("rollout should be parsed");

        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].project_name.as_deref(),
            Some("claude-code-history-viewer")
        );
    }

    #[test]
    #[serial]
    fn load_messages_skips_duplicate_event_msg_for_user_and_agent() {
        // Codex logs user/assistant text in both response_item (type=message)
        // and event_msg (type=user_message / agent_message). Only the
        // response_item version should be kept.
        let tmp = TempDir::new().expect("temp dir should be created");
        let codex_home = tmp.path().join("codex-home");
        let sessions_dir = codex_home.join("sessions");
        fs::create_dir_all(&sessions_dir).expect("sessions dir should be created");
        let _guard = EnvVarGuard::set("CODEX_HOME", &codex_home);
        let rollout_path = sessions_dir.join("rollout-dedup-test.jsonl");

        let lines = [
            json!({
                "timestamp": "2026-03-01T10:00:00Z",
                "type": "session_meta",
                "payload": { "id": "sess-dedup" }
            }),
            // User message via response_item (canonical)
            json!({
                "timestamp": "2026-03-01T10:00:01Z",
                "type": "response_item",
                "payload": {
                    "id": "item-u1",
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": "hello" }]
                }
            }),
            // Duplicate user message via event_msg (should be skipped)
            json!({
                "timestamp": "2026-03-01T10:00:01Z",
                "type": "event_msg",
                "payload": {
                    "type": "user_message",
                    "message": "hello"
                }
            }),
            // Assistant message via response_item (canonical)
            json!({
                "timestamp": "2026-03-01T10:00:02Z",
                "type": "response_item",
                "payload": {
                    "id": "item-a1",
                    "type": "message",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "hi there" }]
                }
            }),
            // Duplicate assistant message via event_msg (should be skipped)
            json!({
                "timestamp": "2026-03-01T10:00:02Z",
                "type": "event_msg",
                "payload": {
                    "type": "agent_message",
                    "message": "hi there"
                }
            }),
            // Non-duplicate event (token_count) should still be processed
            json!({
                "timestamp": "2026-03-01T10:00:03Z",
                "type": "event_msg",
                "payload": {
                    "type": "token_count",
                    "info": {
                        "total_token_usage": {
                            "input_tokens": 50,
                            "output_tokens": 10
                        }
                    }
                }
            }),
        ];

        let content = lines
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&rollout_path, format!("{content}\n")).expect("fixture should be written");

        let messages = load_messages(
            rollout_path
                .to_str()
                .expect("rollout path should be valid UTF-8"),
        )
        .expect("rollout should be parsed");

        // Only 2 messages: 1 user + 1 assistant (no duplicates from event_msg)
        // Before this fix, there were 4 messages (each duplicated by event_msg).
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].message_type, "user");
        assert_eq!(messages[1].message_type, "assistant");

        // Verify content is correct
        let user_text = messages[0]
            .content
            .as_ref()
            .and_then(Value::as_array)
            .and_then(|arr| arr[0].get("text"))
            .and_then(Value::as_str);
        assert_eq!(user_text, Some("hello"));

        let assistant_text = messages[1]
            .content
            .as_ref()
            .and_then(Value::as_array)
            .and_then(|arr| arr[0].get("text"))
            .and_then(Value::as_str);
        assert_eq!(assistant_text, Some("hi there"));

        // token_count event should still be applied to assistant message
        assert_eq!(
            messages[1].usage.as_ref().and_then(|u| u.input_tokens),
            Some(50)
        );
    }

    #[test]
    #[serial]
    fn load_messages_dedup_multi_turn_conversation() {
        // Simulates a realistic multi-turn Codex conversation where each
        // user/assistant message appears as both response_item and event_msg.
        let tmp = TempDir::new().expect("temp dir should be created");
        let codex_home = tmp.path().join("codex-home");
        let sessions_dir = codex_home.join("sessions");
        fs::create_dir_all(&sessions_dir).expect("sessions dir should be created");
        let _guard = EnvVarGuard::set("CODEX_HOME", &codex_home);
        let rollout_path = sessions_dir.join("rollout-multiturn.jsonl");

        let lines = [
            json!({
                "timestamp": "2026-03-01T10:00:00Z",
                "type": "session_meta",
                "payload": { "id": "sess-multi" }
            }),
            // Turn 1: user
            json!({
                "timestamp": "2026-03-01T10:00:01Z",
                "type": "response_item",
                "payload": {
                    "id": "u1", "type": "message", "role": "user",
                    "content": [{ "type": "input_text", "text": "first question" }]
                }
            }),
            json!({
                "timestamp": "2026-03-01T10:00:01Z",
                "type": "event_msg",
                "payload": { "type": "user_message", "message": "first question" }
            }),
            // Turn 1: assistant
            json!({
                "timestamp": "2026-03-01T10:00:02Z",
                "type": "response_item",
                "payload": {
                    "id": "a1", "type": "message", "role": "assistant",
                    "content": [{ "type": "output_text", "text": "first answer" }]
                }
            }),
            json!({
                "timestamp": "2026-03-01T10:00:02Z",
                "type": "event_msg",
                "payload": { "type": "agent_message", "message": "first answer" }
            }),
            // Turn 2: user
            json!({
                "timestamp": "2026-03-01T10:00:03Z",
                "type": "response_item",
                "payload": {
                    "id": "u2", "type": "message", "role": "user",
                    "content": [{ "type": "input_text", "text": "follow-up" }]
                }
            }),
            json!({
                "timestamp": "2026-03-01T10:00:03Z",
                "type": "event_msg",
                "payload": { "type": "user_message", "message": "follow-up" }
            }),
            // Turn 2: assistant
            json!({
                "timestamp": "2026-03-01T10:00:04Z",
                "type": "response_item",
                "payload": {
                    "id": "a2", "type": "message", "role": "assistant",
                    "content": [{ "type": "output_text", "text": "second answer" }]
                }
            }),
            json!({
                "timestamp": "2026-03-01T10:00:04Z",
                "type": "event_msg",
                "payload": { "type": "agent_message", "message": "second answer" }
            }),
            // Turn 3: user (final, no assistant reply yet)
            json!({
                "timestamp": "2026-03-01T10:00:05Z",
                "type": "response_item",
                "payload": {
                    "id": "u3", "type": "message", "role": "user",
                    "content": [{ "type": "input_text", "text": "one more thing" }]
                }
            }),
            json!({
                "timestamp": "2026-03-01T10:00:05Z",
                "type": "event_msg",
                "payload": { "type": "user_message", "message": "one more thing" }
            }),
        ];

        let content = lines
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&rollout_path, format!("{content}\n")).expect("fixture should be written");

        let messages = load_messages(
            rollout_path
                .to_str()
                .expect("rollout path should be valid UTF-8"),
        )
        .expect("rollout should be parsed");

        // 5 messages: user, assistant, user, assistant, user (no duplicates)
        // Without the fix this would be 10 messages.
        assert_eq!(messages.len(), 5);

        let expected = [
            ("user", "first question"),
            ("assistant", "first answer"),
            ("user", "follow-up"),
            ("assistant", "second answer"),
            ("user", "one more thing"),
        ];
        for (i, (msg_type, text)) in expected.iter().enumerate() {
            assert_eq!(messages[i].message_type, *msg_type, "message {i} type");
            let actual_text = messages[i]
                .content
                .as_ref()
                .and_then(Value::as_array)
                .and_then(|arr| arr[0].get("text"))
                .and_then(Value::as_str);
            assert_eq!(actual_text, Some(*text), "message {i} content");
        }
    }

    #[test]
    #[serial]
    fn load_sessions_includes_archived_sessions() {
        invalidate_session_index_cache();
        let tmp = TempDir::new().expect("temp dir should be created");
        let codex_home = tmp.path().join("codex-home");
        let sessions_dir = codex_home
            .join("sessions")
            .join("2026")
            .join("02")
            .join("21");
        let archived_dir = codex_home.join("archived_sessions");
        fs::create_dir_all(&sessions_dir).expect("sessions dir should be created");
        fs::create_dir_all(&archived_dir).expect("archived dir should be created");
        let _guard = EnvVarGuard::set("CODEX_HOME", &codex_home);

        let project_cwd = "/Users/jack/client/claude-code-history-viewer";
        let active_rollout = sessions_dir.join("rollout-active.jsonl");
        let archived_rollout = archived_dir.join("rollout-archived.jsonl");
        let active_lines = [
            json!({
                "type": "session_meta",
                "payload": { "id": "active-session", "cwd": project_cwd }
            }),
            json!({
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "created_at": "2026-02-21T10:00:00Z",
                    "content": [{ "type": "input_text", "text": "active" }]
                }
            }),
        ];
        let archived_lines = [
            json!({
                "type": "session_meta",
                "payload": { "id": "archived-session", "cwd": project_cwd }
            }),
            json!({
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "created_at": "2026-02-21T11:00:00Z",
                    "content": [{ "type": "input_text", "text": "archived" }]
                }
            }),
        ];
        let active_content = active_lines
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let archived_content = archived_lines
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&active_rollout, format!("{active_content}\n"))
            .expect("active fixture should be written");
        fs::write(&archived_rollout, format!("{archived_content}\n"))
            .expect("archived fixture should be written");

        let sessions = load_sessions(&format!("codex://{project_cwd}"), false)
            .expect("sessions should be loaded");

        assert_eq!(sessions.len(), 2);
        assert!(sessions.iter().any(|s| s.file_path.contains("/sessions/")));
        assert!(sessions
            .iter()
            .any(|s| s.file_path.contains("/archived_sessions/")));
        invalidate_session_index_cache();
    }

    #[test]
    #[serial]
    fn codex_session_filters_hide_permission_guardian_sessions() {
        invalidate_session_index_cache();
        let tmp = TempDir::new().expect("temp dir should be created");
        let codex_home = tmp.path().join("codex-home");
        let sessions_dir = codex_home
            .join("sessions")
            .join("2026")
            .join("06")
            .join("13");
        fs::create_dir_all(&sessions_dir).expect("sessions dir should be created");
        let _guard = EnvVarGuard::set("CODEX_HOME", &codex_home);

        let project_cwd = "/Users/jack/client/filter-project";
        let normal_rollout = sessions_dir.join("rollout-normal.jsonl");
        let permissions_rollout = sessions_dir.join("rollout-permissions.jsonl");
        let normal_lines = [
            json!({
                "type": "session_meta",
                "payload": { "id": "normal-session", "cwd": project_cwd }
            }),
            json!({
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "created_at": "2026-06-13T10:00:00Z",
                    "content": [{ "type": "input_text", "text": "normal prompt" }]
                }
            }),
        ];
        let permissions_lines = [
            json!({
                "type": "session_meta",
                "payload": {
                    "id": "permissions-session",
                    "cwd": project_cwd,
                    "source": { "subagent": { "other": "guardian" } },
                    "base_instructions": {
                        "text": "You are judging one planned coding-agent action."
                    }
                }
            }),
            json!({
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "created_at": "2026-06-13T11:00:00Z",
                    "content": [{ "type": "input_text", "text": "permissions instructions" }]
                }
            }),
        ];
        fs::write(
            &normal_rollout,
            normal_lines
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n")
                + "\n",
        )
        .expect("normal fixture should be written");
        fs::write(
            &permissions_rollout,
            permissions_lines
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n")
                + "\n",
        )
        .expect("permissions fixture should be written");

        let filters = CodexSessionFilters {
            enabled: true,
            include_permissions: false,
            include_git_commit_subagents: false,
        };
        let projects =
            scan_projects_from_path_with_filters(&codex_home.to_string_lossy(), Some(&filters))
                .expect("projects should be scanned");
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].session_count, 1);

        let sessions =
            load_sessions_with_filters(&format!("codex://{project_cwd}"), false, Some(&filters))
                .expect("sessions should be loaded");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].actual_session_id, "normal-session");

        let visible_filters = CodexSessionFilters {
            enabled: true,
            include_permissions: true,
            include_git_commit_subagents: false,
        };
        let visible_sessions = load_sessions_with_filters(
            &format!("codex://{project_cwd}"),
            false,
            Some(&visible_filters),
        )
        .expect("sessions should be loaded");
        assert_eq!(visible_sessions.len(), 2);
        invalidate_session_index_cache();
    }

    #[test]
    #[serial]
    fn codex_session_filters_hide_ajk_git_commit_subagent_sessions() {
        invalidate_session_index_cache();
        let tmp = TempDir::new().expect("temp dir should be created");
        let codex_home = tmp.path().join("codex-home");
        let sessions_dir = codex_home
            .join("sessions")
            .join("2026")
            .join("06")
            .join("14");
        fs::create_dir_all(&sessions_dir).expect("sessions dir should be created");
        let _guard = EnvVarGuard::set("CODEX_HOME", &codex_home);

        let project_cwd = "/Users/jack/client/git-commit-project";
        let normal_rollout = sessions_dir.join("rollout-normal.jsonl");
        let git_commit_rollout = sessions_dir.join("rollout-git-commit-worker.jsonl");
        let normal_lines = [
            json!({
                "type": "session_meta",
                "payload": { "id": "normal-session", "cwd": project_cwd }
            }),
            json!({
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "created_at": "2026-06-14T10:00:00Z",
                    "content": [{ "type": "input_text", "text": "normal prompt" }]
                }
            }),
        ];
        let git_commit_lines = [
            json!({
                "type": "session_meta",
                "payload": {
                    "id": "git-commit-session",
                    "cwd": project_cwd,
                    "source": {
                        "subagent": {
                            "thread_spawn": {
                                "parent_thread_id": "parent-session",
                                "depth": 1,
                                "agent_nickname": "Sagan",
                                "agent_role": "worker"
                            }
                        }
                    },
                    "thread_source": "subagent",
                    "agent_nickname": "Sagan",
                    "agent_role": "worker"
                }
            }),
            json!({
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "created_at": "2026-06-14T11:00:00Z",
                    "content": [{
                        "type": "input_text",
                        "text": "你负责在仓库 /tmp/repo 执行一次 git commit。请严格按 `/Users/Ruan/.cc-switch/skills/ajk-git-commit/SKILL.md` 的规则。"
                    }]
                }
            }),
        ];
        fs::write(
            &normal_rollout,
            normal_lines
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n")
                + "\n",
        )
        .expect("normal fixture should be written");
        fs::write(
            &git_commit_rollout,
            git_commit_lines
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n")
                + "\n",
        )
        .expect("git commit worker fixture should be written");

        let default_projects =
            scan_projects_from_path_with_filters(&codex_home.to_string_lossy(), None)
                .expect("projects should be scanned");
        assert_eq!(default_projects.len(), 1);
        assert_eq!(default_projects[0].session_count, 1);

        let default_sessions =
            load_sessions_with_filters(&format!("codex://{project_cwd}"), false, None)
                .expect("sessions should be loaded");
        assert_eq!(default_sessions.len(), 1);
        assert_eq!(default_sessions[0].actual_session_id, "normal-session");

        let default_search_results =
            search_with_filters(AJK_GIT_COMMIT_SKILL_NAME, 10, SearchScope::All, None)
                .expect("search should run");
        assert!(default_search_results.is_empty());

        let filters = CodexSessionFilters {
            enabled: true,
            include_permissions: true,
            include_git_commit_subagents: false,
        };
        let projects =
            scan_projects_from_path_with_filters(&codex_home.to_string_lossy(), Some(&filters))
                .expect("projects should be scanned");
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].session_count, 1);

        let sessions =
            load_sessions_with_filters(&format!("codex://{project_cwd}"), false, Some(&filters))
                .expect("sessions should be loaded");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].actual_session_id, "normal-session");

        let hidden_search_results = search_with_filters(
            AJK_GIT_COMMIT_SKILL_NAME,
            10,
            SearchScope::All,
            Some(&filters),
        )
        .expect("search should run");
        assert!(hidden_search_results.is_empty());

        let visible_filters = CodexSessionFilters {
            enabled: true,
            include_permissions: true,
            include_git_commit_subagents: true,
        };
        let visible_sessions = load_sessions_with_filters(
            &format!("codex://{project_cwd}"),
            false,
            Some(&visible_filters),
        )
        .expect("sessions should be loaded");
        assert_eq!(visible_sessions.len(), 2);

        let visible_search_results = search_with_filters(
            AJK_GIT_COMMIT_SKILL_NAME,
            10,
            SearchScope::All,
            Some(&visible_filters),
        )
        .expect("search should run");
        assert_eq!(visible_search_results.len(), 1);
        invalidate_session_index_cache();
    }

    #[test]
    #[serial]
    fn scan_projects_populates_session_index_for_fast_loads() {
        invalidate_session_index_cache();
        let tmp = TempDir::new().expect("temp dir should be created");
        let codex_home = tmp.path().join("codex-home");
        let sessions_dir = codex_home
            .join("sessions")
            .join("2026")
            .join("02")
            .join("21");
        fs::create_dir_all(&sessions_dir).expect("sessions dir should be created");
        let _guard = EnvVarGuard::set("CODEX_HOME", &codex_home);

        let project_cwd = "/Users/jack/client/fast-project";
        let rollout_path = sessions_dir.join("rollout-fast-load.jsonl");
        let lines = [
            json!({
                "type": "session_meta",
                "payload": { "id": "fast-session", "cwd": project_cwd }
            }),
            json!({
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "created_at": "2026-02-21T10:00:00Z",
                    "content": [{ "type": "input_text", "text": "fallback title" }]
                }
            }),
        ];
        let content = lines
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&rollout_path, format!("{content}\n")).expect("fixture should be written");
        fs::write(
            codex_home.join("session_index.jsonl"),
            json!({
                "id": "fast-session",
                "thread_name": "Renamed fast session",
                "updated_at": "2026-02-21T10:01:00Z"
            })
            .to_string()
                + "\n",
        )
        .expect("thread title index should be written");

        let projects = scan_projects_from_path(&codex_home.to_string_lossy())
            .expect("projects should be scanned");
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].path, format!("codex://{project_cwd}"));

        fs::remove_file(&rollout_path).expect("fixture should be removable after scan");
        let sessions = load_sessions(&format!("codex://{project_cwd}"), false)
            .expect("sessions should be loaded from cache");

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].summary.as_deref(), Some("Renamed fast session"));
        assert!(sessions[0].is_renamed);
        invalidate_session_index_cache();
    }

    #[test]
    #[serial]
    fn load_messages_accepts_archived_session_path() {
        let tmp = TempDir::new().expect("temp dir should be created");
        let codex_home = tmp.path().join("codex-home");
        let archived_dir = codex_home.join("archived_sessions");
        fs::create_dir_all(&archived_dir).expect("archived dir should be created");
        let _guard = EnvVarGuard::set("CODEX_HOME", &codex_home);
        let rollout_path = archived_dir.join("rollout-archived-only.jsonl");
        let lines = [
            json!({
                "type": "session_meta",
                "payload": { "id": "archived-session", "cwd": "/tmp/project" }
            }),
            json!({
                "type": "response_item",
                "payload": {
                    "id": "item-1",
                    "type": "message",
                    "role": "assistant",
                    "created_at": "2026-02-21T10:00:00Z",
                    "content": [{ "type": "output_text", "text": "ok" }]
                }
            }),
        ];
        let content = lines
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&rollout_path, format!("{content}\n")).expect("fixture should be written");

        let messages = load_messages(
            rollout_path
                .to_str()
                .expect("rollout path should be valid UTF-8"),
        )
        .expect("archived rollout should be parsed");

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].session_id, "archived-session");
    }

    /// Helper: write `lines` as one JSON-per-line into a fresh rollout file
    /// and run `extract_session_info` against it. Returns the resulting
    /// `SessionInfo`. Used by the env-context-skip tests below.
    fn run_extract_session_info_on_lines(lines: Vec<Value>) -> SessionInfo {
        let tmp = TempDir::new().expect("temp dir should be created");
        let rollout_path = tmp.path().join("rollout-2026-05-13.jsonl");
        let body = lines
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&rollout_path, format!("{body}\n")).expect("rollout fixture should be written");
        extract_session_info(&rollout_path).expect("extract_session_info should succeed")
    }

    fn session_meta_line() -> Value {
        json!({
            "timestamp": "2026-05-13T08:00:00Z",
            "type": "session_meta",
            "payload": { "id": "sess-env-ctx", "cwd": "/tmp/proj" }
        })
    }

    fn user_message_line(timestamp: &str, text: &str) -> Value {
        json!({
            "timestamp": timestamp,
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": text }]
            }
        })
    }

    const ENV_CONTEXT_BLOCK: &str = "<environment_context>\n  <cwd>/tmp/proj</cwd>\n  <shell>powershell</shell>\n  <current_date>2026-05-13</current_date>\n  <timezone>Asia/Shanghai</timezone>\n</environment_context>";
    const AGENTS_INSTRUCTIONS_BLOCK: &str = r"# AGENTS.md instructions for /Users/Ruan/Downloads/01_src/github_refs/01-AI/04-AI应用/03-桌面应用/claude-code-history-viewer

<INSTRUCTIONS>
# Codex/Agents 项目管理指南

- 长期实现计划必须遵循 `long-term-plan` skill；若本文件与该 skill 冲突，以 skill 为准。
</INSTRUCTIONS>";

    #[test]
    /// First user message is an auto-injected `<environment_context>` block;
    /// second user message is a real prompt — the summary should be the
    /// real prompt, not the env-context block.
    fn extract_session_info_skips_environment_context_wrapper() {
        let info = run_extract_session_info_on_lines(vec![
            session_meta_line(),
            user_message_line("2026-05-13T08:00:01Z", ENV_CONTEXT_BLOCK),
            user_message_line(
                "2026-05-13T08:00:02Z",
                "Please review my PR for the Antigravity provider.",
            ),
        ]);

        assert_eq!(
            info.summary.as_deref(),
            Some("Please review my PR for the Antigravity provider.")
        );
        // message_count counts *every* response_item type=message,
        // including the skipped wrapper, so the count surfaces real
        // activity volume.
        assert_eq!(info.message_count, 2);
    }

    #[test]
    /// First user message is a real prompt — extractor must not regress
    /// pre-existing behaviour for sessions without an env-context wrapper.
    fn extract_session_info_uses_first_real_user_prompt() {
        let info = run_extract_session_info_on_lines(vec![
            session_meta_line(),
            user_message_line("2026-05-13T08:00:01Z", "fix the WSL crash"),
            user_message_line("2026-05-13T08:00:02Z", "second message"),
        ]);

        assert_eq!(info.summary.as_deref(), Some("fix the WSL crash"));
        assert_eq!(info.message_count, 2);
    }

    #[test]
    fn load_thread_titles_from_index_uses_latest_thread_name() {
        let tmp = TempDir::new().expect("temp dir should be created");
        let index_path = tmp.path().join("session_index.jsonl");
        fs::write(
            &index_path,
            [
                r#"{"id":"sess-1","thread_name":"old title","updated_at":"2026-06-13T14:07:37Z"}"#,
                r#"{"id":"sess-1","thread_name":"new title","updated_at":"2026-06-14T02:02:34Z"}"#,
                r#"{"id":"sess-2","thread_name":"other title","updated_at":"2026-06-13T14:07:37Z"}"#,
            ]
            .join("\n"),
        )
        .expect("index fixture should be written");

        let titles = load_thread_titles_from_index(&index_path).expect("index should parse");

        assert_eq!(titles["sess-1"].name, "new title");
        assert_eq!(titles["sess-2"].name, "other title");
    }

    #[test]
    /// First user message is an auto-injected AGENTS.md instruction block;
    /// second user message is a real prompt, so the summary should be the
    /// real prompt.
    fn extract_session_info_skips_agents_instructions_block() {
        let info = run_extract_session_info_on_lines(vec![
            session_meta_line(),
            user_message_line("2026-05-13T08:00:01Z", AGENTS_INSTRUCTIONS_BLOCK),
            user_message_line(
                "2026-05-13T08:00:02Z",
                "Optimize the Codex conversation title.",
            ),
        ]);

        assert_eq!(
            info.summary.as_deref(),
            Some("Optimize the Codex conversation title.")
        );
        assert_eq!(info.message_count, 2);
    }

    #[test]
    /// Session contains only an auto-injected AGENTS.md block and no real
    /// prompt, so it should not get a misleading summary.
    fn extract_session_info_agents_instructions_only_yields_no_summary() {
        let info = run_extract_session_info_on_lines(vec![
            session_meta_line(),
            user_message_line("2026-05-13T08:00:01Z", AGENTS_INSTRUCTIONS_BLOCK),
        ]);

        assert!(
            info.summary.is_none(),
            "AGENTS-only sessions should not produce a misleading summary; got {:?}",
            info.summary
        );
        assert_eq!(info.message_count, 1);
    }

    #[test]
    /// A normal user prompt can mention the AGENTS.md heading without being
    /// the injected instruction block. Require the instruction marker to avoid
    /// skipping real prompts.
    fn extract_session_info_keeps_real_prompt_that_mentions_agents_heading() {
        let info = run_extract_session_info_on_lines(vec![
            session_meta_line(),
            user_message_line(
                "2026-05-13T08:00:01Z",
                "Please summarize # AGENTS.md instructions for this repo for onboarding",
            ),
            user_message_line("2026-05-13T08:00:02Z", "second message"),
        ]);

        assert_eq!(
            info.summary.as_deref(),
            Some("Please summarize # AGENTS.md instructions for this repo for onboarding")
        );
        assert_eq!(info.message_count, 2);
    }

    #[test]
    /// VS Code can wrap the real prompt in a large IDE context block. Extract
    /// the final Codex request heading instead of using the wrapper heading as
    /// the session summary.
    fn extract_session_info_extracts_vscode_ide_request() {
        let info = run_extract_session_info_on_lines(vec![
            session_meta_line(),
            user_message_line(
                "2026-05-13T08:00:01Z",
                "# Context from my IDE setup:\n\n## Active file: src/main.ts\n\n## My request for Codex:\nFix the session title preview",
            ),
        ]);

        assert_eq!(
            info.summary.as_deref(),
            Some("Fix the session title preview")
        );
        assert_eq!(info.message_count, 1);
    }

    #[test]
    /// Inline request headings are also supported by Codex's VS Code context.
    fn extract_session_info_extracts_inline_vscode_ide_request() {
        let info = run_extract_session_info_on_lines(vec![
            session_meta_line(),
            user_message_line(
                "2026-05-13T08:00:01Z",
                "# Context from my IDE setup:\n\n## My request for Codex: Fix the TOC preview",
            ),
        ]);

        assert_eq!(info.summary.as_deref(), Some("Fix the TOC preview"));
        assert_eq!(info.message_count, 1);
    }

    #[test]
    /// Use the last request heading because earlier headings can be present in
    /// active selection text.
    fn extract_session_info_uses_last_vscode_request_heading() {
        let info = run_extract_session_info_on_lines(vec![
            session_meta_line(),
            user_message_line(
                "2026-05-13T08:00:01Z",
                "# Context from my IDE setup:\n\n## Active selection:\n## My request for Codex:\nselected document content, not the prompt\n\n## My request for Codex:\nUse the real request heading",
            ),
        ]);

        assert_eq!(
            info.summary.as_deref(),
            Some("Use the real request heading")
        );
        assert_eq!(info.message_count, 1);
    }

    #[test]
    /// If VS Code context has no Codex request section, keep scanning for the
    /// next real user prompt.
    fn extract_session_info_skips_vscode_context_without_request() {
        let info = run_extract_session_info_on_lines(vec![
            session_meta_line(),
            user_message_line(
                "2026-05-13T08:00:01Z",
                "# Context from my IDE setup:\n\n## Active file: src/main.ts",
            ),
            user_message_line("2026-05-13T08:00:02Z", "Fix the login bug"),
        ]);

        assert_eq!(info.summary.as_deref(), Some("Fix the login bug"));
        assert_eq!(info.message_count, 2);
    }

    #[test]
    /// Session contains only auto-injected wrapper messages and no real
    /// prompt — summary stays None, matching legacy empty-session behaviour.
    fn extract_session_info_env_context_only_yields_no_summary() {
        let info = run_extract_session_info_on_lines(vec![
            session_meta_line(),
            user_message_line("2026-05-13T08:00:01Z", ENV_CONTEXT_BLOCK),
        ]);

        assert!(
            info.summary.is_none(),
            "env-context-only sessions should not produce a misleading summary; got {:?}",
            info.summary
        );
        // The wrapper still counts as a message — only the summary is gated.
        assert_eq!(info.message_count, 1);
    }
}
