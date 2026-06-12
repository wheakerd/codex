//! Session picker loading, State DB seeding, and authoritative reconciliation.

use std::io;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::NaiveDateTime;
use codex_app_server_protocol::Thread;
use codex_app_server_protocol::ThreadListCwdFilter;
use codex_app_server_protocol::ThreadListParams;
use codex_app_server_protocol::ThreadSortKey;
use codex_protocol::ThreadId;
use codex_protocol::protocol::SessionSource;
use tokio::sync::mpsc;
use tracing::warn;

use super::AppServerSession;
use super::LoadingState;
use super::PickerState;
use super::ProviderFilter;
use super::RawReasoningVisibility;
use super::Row;
use super::SessionTarget;
use super::SessionTranscriptState;
use super::TranscriptCells;
use super::TranscriptPreviewLine;
use super::TranscriptPreviewState;
use super::load_session_transcript;
use super::load_transcript_preview;
use super::parse_timestamp_str;
use super::paths_match;

const PAGE_SIZE: usize = 25;

#[derive(Clone)]
pub(super) struct PageLoadRequest {
    pub(super) cursor: Option<PageCursor>,
    pub(super) request_token: usize,
    pub(super) search_token: Option<usize>,
    pub(super) cwd_filter: Option<PathBuf>,
    pub(super) provider_filter: ProviderFilter,
    pub(super) sort_key: ThreadSortKey,
    pub(super) seed_from_state_db: bool,
}

pub(super) enum PickerLoadRequest {
    Page(PageLoadRequest),
    Preview { thread_id: ThreadId },
    Transcript { thread_id: ThreadId },
}

pub(super) type PickerLoader = Arc<dyn Fn(PickerLoadRequest) + Send + Sync>;

pub(super) enum BackgroundEvent {
    SeedPage {
        request_token: usize,
        page: PickerPage,
    },
    Page {
        request_token: usize,
        search_token: Option<usize>,
        page: io::Result<PickerPage>,
    },
    Preview {
        thread_id: ThreadId,
        preview: io::Result<Vec<TranscriptPreviewLine>>,
    },
    Transcript {
        thread_id: ThreadId,
        transcript: io::Result<TranscriptCells>,
    },
}

#[derive(Clone)]
pub(super) enum PageCursor {
    AppServer(String),
}

pub(super) struct PickerPage {
    pub(super) rows: Vec<Row>,
    pub(super) next_cursor: Option<PageCursor>,
    pub(super) num_scanned_files: usize,
    pub(super) reached_scan_cap: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum InitialPageLoad {
    #[default]
    Authoritative,
    SeedPending,
    Provisional,
}

impl InitialPageLoad {
    pub(super) fn state_db_first() -> Self {
        Self::SeedPending
    }

    pub(super) fn begin_load(&mut self) -> bool {
        let seed_from_state_db = *self == Self::SeedPending;
        *self = Self::Authoritative;
        seed_from_state_db
    }

    fn mark_seeded(&mut self) {
        *self = Self::Provisional;
    }

    fn finish_reconciliation(&mut self) -> bool {
        let was_provisional = *self == Self::Provisional;
        *self = Self::Authoritative;
        was_provisional
    }

    pub(super) fn is_provisional(self) -> bool {
        self == Self::Provisional
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ThreadListLookupMode {
    StateDbOnly,
    ScanAndRepair,
}

pub(super) struct SelectionValidation {
    pub(super) path: PathBuf,
    pub(super) thread_id: ThreadId,
    pub(super) thread_name: Option<String>,
    pub(super) git_branch: Option<String>,
    pub(super) codex_home: PathBuf,
    pub(super) cwd_filter: Option<PathBuf>,
    pub(super) provider_filter: ProviderFilter,
    pub(super) include_non_interactive: bool,
    pub(super) query: String,
}

pub(super) fn spawn_app_server_page_loader(
    app_server: AppServerSession,
    include_non_interactive: bool,
    raw_reasoning_visibility: RawReasoningVisibility,
    bg_tx: mpsc::UnboundedSender<BackgroundEvent>,
) -> PickerLoader {
    let (request_tx, mut request_rx) = mpsc::unbounded_channel::<PickerLoadRequest>();

    tokio::spawn(async move {
        let mut app_server = app_server;
        while let Some(request) = request_rx.recv().await {
            match request {
                PickerLoadRequest::Page(request) => {
                    if request.seed_from_state_db {
                        match load_app_server_page(
                            &mut app_server,
                            /*cursor*/ None,
                            request.cwd_filter.as_deref(),
                            request.provider_filter.clone(),
                            request.sort_key,
                            include_non_interactive,
                            ThreadListLookupMode::StateDbOnly,
                        )
                        .await
                        {
                            Ok(page) => {
                                let _ = bg_tx.send(BackgroundEvent::SeedPage {
                                    request_token: request.request_token,
                                    page,
                                });
                            }
                            Err(err) => {
                                warn!(
                                    %err,
                                    "State DB picker lookup failed; falling back to scan-and-repair"
                                );
                            }
                        }
                    }

                    let cursor = request.cursor.map(|PageCursor::AppServer(cursor)| cursor);
                    let page = load_app_server_page(
                        &mut app_server,
                        cursor,
                        request.cwd_filter.as_deref(),
                        request.provider_filter,
                        request.sort_key,
                        include_non_interactive,
                        ThreadListLookupMode::ScanAndRepair,
                    )
                    .await;
                    let _ = bg_tx.send(BackgroundEvent::Page {
                        request_token: request.request_token,
                        search_token: request.search_token,
                        page,
                    });
                }
                PickerLoadRequest::Preview { thread_id } => {
                    let preview = load_transcript_preview(&mut app_server, thread_id).await;
                    let _ = bg_tx.send(BackgroundEvent::Preview { thread_id, preview });
                }
                PickerLoadRequest::Transcript { thread_id } => {
                    let transcript = load_session_transcript(
                        &mut app_server,
                        thread_id,
                        raw_reasoning_visibility,
                    )
                    .await;
                    let _ = bg_tx.send(BackgroundEvent::Transcript {
                        thread_id,
                        transcript,
                    });
                }
            }
        }
        if let Err(err) = app_server.shutdown().await {
            warn!(%err, "Failed to shut down app-server picker session");
        }
    });

    Arc::new(move |request: PickerLoadRequest| {
        let _ = request_tx.send(request);
    })
}

async fn load_app_server_page(
    app_server: &mut AppServerSession,
    cursor: Option<String>,
    cwd_filter: Option<&Path>,
    provider_filter: ProviderFilter,
    sort_key: ThreadSortKey,
    include_non_interactive: bool,
    lookup_mode: ThreadListLookupMode,
) -> io::Result<PickerPage> {
    let response = app_server
        .thread_list(thread_list_params(
            cursor,
            cwd_filter,
            provider_filter,
            sort_key,
            include_non_interactive,
            lookup_mode,
        ))
        .await
        .map_err(io::Error::other)?;
    let num_scanned_files = response.data.len();

    Ok(PickerPage {
        rows: response
            .data
            .into_iter()
            .filter_map(row_from_app_server_thread)
            .collect(),
        next_cursor: response.next_cursor.map(PageCursor::AppServer),
        num_scanned_files,
        reached_scan_cap: false,
    })
}

fn row_from_app_server_thread(thread: Thread) -> Option<Row> {
    let thread_id = match ThreadId::from_string(&thread.id) {
        Ok(thread_id) => thread_id,
        Err(err) => {
            warn!(thread_id = thread.id, %err, "Skipping app-server picker row with invalid id");
            return None;
        }
    };
    Some(Row::from_app_server_thread(&thread, thread_id))
}

impl Row {
    fn from_app_server_thread(thread: &Thread, thread_id: ThreadId) -> Self {
        let preview = thread.preview.trim();
        Self {
            path: thread.path.clone(),
            preview: if preview.is_empty() {
                String::from("(no message yet)")
            } else {
                preview.to_string()
            },
            thread_id: Some(thread_id),
            thread_name: thread.name.clone(),
            created_at: chrono::DateTime::from_timestamp(thread.created_at, 0)
                .map(|dt| dt.with_timezone(&chrono::Utc)),
            updated_at: chrono::DateTime::from_timestamp(thread.updated_at, 0)
                .map(|dt| dt.with_timezone(&chrono::Utc)),
            cwd: Some(thread.cwd.to_path_buf()),
            git_branch: thread
                .git_info
                .as_ref()
                .and_then(|git_info| git_info.branch.clone()),
        }
    }
}

fn thread_list_params(
    cursor: Option<String>,
    cwd_filter: Option<&Path>,
    provider_filter: ProviderFilter,
    sort_key: ThreadSortKey,
    include_non_interactive: bool,
    lookup_mode: ThreadListLookupMode,
) -> ThreadListParams {
    ThreadListParams {
        cursor,
        limit: Some(PAGE_SIZE as u32),
        sort_key: Some(sort_key),
        sort_direction: None,
        model_providers: match provider_filter {
            ProviderFilter::Any => None,
            ProviderFilter::MatchDefault(default_provider) => Some(vec![default_provider]),
        },
        source_kinds: Some(crate::resume_source_kinds(include_non_interactive)),
        archived: Some(false),
        cwd: cwd_filter.map(|cwd| ThreadListCwdFilter::One(cwd.to_string_lossy().into_owned())),
        use_state_db_only: lookup_mode == ThreadListLookupMode::StateDbOnly,
        search_term: None,
    }
}

/// Validates a selected provisional row against the same rollout summary used
/// by scan-and-repair before allowing a resume or fork.
pub(super) async fn validate_provisional_session_target(
    input: SelectionValidation,
) -> io::Result<SessionTarget> {
    let rollout_path = codex_rollout::existing_rollout_path(input.path.as_path())
        .await
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "selected session rollout no longer exists",
            )
        })?;
    if !is_discoverable_active_rollout_path(&input.codex_home, &rollout_path) {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "selected session is not an active rollout",
        ));
    }
    let item = codex_rollout::read_thread_item_from_rollout(rollout_path.clone())
        .await
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "selected session rollout is not eligible for the picker",
            )
        })?;
    let provider_matches = match &input.provider_filter {
        ProviderFilter::Any => true,
        ProviderFilter::MatchDefault(default_provider) => item
            .model_provider
            .as_deref()
            .is_none_or(|provider| provider == default_provider),
    };
    let source_matches = match item.source.as_ref() {
        Some(SessionSource::Cli | SessionSource::VSCode) => true,
        Some(SessionSource::Exec | SessionSource::Mcp) => input.include_non_interactive,
        Some(
            SessionSource::Custom(_)
            | SessionSource::Internal(_)
            | SessionSource::SubAgent(_)
            | SessionSource::Unknown,
        )
        | None => false,
    };
    let cwd_matches = input.cwd_filter.as_ref().is_none_or(|filter| {
        item.cwd
            .as_ref()
            .is_some_and(|cwd| paths_match(cwd, filter))
    });
    let row = row_from_rollout_item(
        item,
        rollout_path.clone(),
        input.thread_name,
        input.git_branch,
    )
    .ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "selected session rollout is not eligible for the picker",
        )
    })?;
    let query = input.query.to_lowercase();
    if row.thread_id != Some(input.thread_id)
        || !provider_matches
        || !source_matches
        || !cwd_matches
        || (!query.is_empty() && !row.matches_query(&query))
    {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "selected session no longer matches the picker",
        ));
    }

    Ok(SessionTarget {
        path: Some(rollout_path),
        thread_id: input.thread_id,
    })
}

fn is_discoverable_active_rollout_path(codex_home: &Path, path: &Path) -> bool {
    let Ok(relative_path) = path.strip_prefix(codex_home.join(codex_rollout::SESSIONS_SUBDIR))
    else {
        return false;
    };
    let mut components = relative_path.components();
    let (
        Some(Component::Normal(year)),
        Some(Component::Normal(month)),
        Some(Component::Normal(day)),
        Some(Component::Normal(file_name)),
        None,
    ) = (
        components.next(),
        components.next(),
        components.next(),
        components.next(),
        components.next(),
    )
    else {
        return false;
    };
    year.to_str()
        .and_then(|year| year.parse::<u16>().ok())
        .is_some()
        && month
            .to_str()
            .and_then(|month| month.parse::<u8>().ok())
            .is_some()
        && day
            .to_str()
            .and_then(|day| day.parse::<u8>().ok())
            .is_some()
        && file_name.to_str().is_some_and(is_rollout_file_name)
}

fn is_rollout_file_name(file_name: &str) -> bool {
    let file_name = file_name.strip_suffix(".zst").unwrap_or(file_name);
    let Some(core) = file_name
        .strip_prefix("rollout-")
        .and_then(|name| name.strip_suffix(".jsonl"))
    else {
        return false;
    };
    let Some(separator_index) = core.len().checked_sub(37) else {
        return false;
    };
    if core.as_bytes().get(separator_index) != Some(&b'-') {
        return false;
    }
    let timestamp = &core[..separator_index];
    let thread_id = &core[separator_index + 1..];
    NaiveDateTime::parse_from_str(timestamp, "%Y-%m-%dT%H-%M-%S").is_ok()
        && ThreadId::from_string(thread_id).is_ok()
}

fn row_from_rollout_item(
    item: codex_rollout::ThreadItem,
    path: PathBuf,
    thread_name: Option<String>,
    git_branch: Option<String>,
) -> Option<Row> {
    let thread_id = item.thread_id?;
    let preview = item.preview.or(item.first_user_message)?;
    let preview = preview.trim();
    Some(Row {
        path: Some(path),
        preview: if preview.is_empty() {
            String::from("(no message yet)")
        } else {
            preview.to_string()
        },
        thread_id: Some(thread_id),
        thread_name,
        created_at: parse_timestamp_str(item.created_at.as_deref().unwrap_or_default()),
        updated_at: parse_timestamp_str(item.updated_at.as_deref().unwrap_or_default()),
        cwd: item.cwd,
        git_branch: git_branch.or(item.git_branch),
    })
}

impl PickerState {
    pub(super) async fn handle_background_event(
        &mut self,
        event: BackgroundEvent,
    ) -> color_eyre::eyre::Result<()> {
        match event {
            BackgroundEvent::SeedPage {
                request_token,
                page,
            } => {
                let LoadingState::Pending(pending) = self.pagination.loading else {
                    return Ok(());
                };
                if pending.request_token != request_token {
                    return Ok(());
                }
                self.initial_page_load.mark_seeded();
                self.replace_with_page(page);
            }
            BackgroundEvent::Page {
                request_token,
                search_token,
                page,
            } => {
                let pending = match self.pagination.loading {
                    LoadingState::Pending(pending) => pending,
                    LoadingState::Idle => return Ok(()),
                };
                if pending.request_token != request_token {
                    return Ok(());
                }
                self.pagination.loading = LoadingState::Idle;
                match page {
                    Ok(page) if self.initial_page_load.finish_reconciliation() => {
                        self.replace_with_page(page);
                        self.complete_pending_page_down();
                        self.reevaluate_search();
                    }
                    Ok(page) => {
                        self.ingest_page(page);
                        self.complete_pending_page_down();
                        let completed_token = pending.search_token.or(search_token);
                        self.continue_search_if_token_matches(completed_token);
                    }
                    Err(err) if self.initial_page_load.is_provisional() => {
                        warn!(
                            %err,
                            "Session picker reconciliation failed; keeping State DB results"
                        );
                        let cached_results_are_truncated = self.pagination.next_cursor.is_some();
                        self.pagination.next_cursor = None;
                        self.inline_error = Some(if cached_results_are_truncated {
                            String::from(
                                "Could not refresh sessions; showing the first page of indexed results",
                            )
                        } else {
                            String::from("Could not refresh sessions; showing indexed results")
                        });
                        self.complete_pending_page_down();
                        self.reevaluate_search();
                        self.request_frame();
                    }
                    Err(err) => return Err(color_eyre::Report::from(err)),
                }
            }
            BackgroundEvent::Preview { thread_id, preview } => {
                self.transcript_previews.insert(
                    thread_id,
                    match preview {
                        Ok(lines) => TranscriptPreviewState::Loaded(lines),
                        Err(_) => TranscriptPreviewState::Failed,
                    },
                );
                self.request_frame();
            }
            BackgroundEvent::Transcript {
                thread_id,
                transcript,
            } => match transcript {
                Ok(cells) => {
                    let should_open = self.pending_transcript_open == Some(thread_id);
                    self.transcript_cells
                        .insert(thread_id, SessionTranscriptState::Loaded(cells.clone()));
                    if should_open {
                        self.open_pending_transcript_if_ready();
                    }
                    self.request_frame();
                }
                Err(_) => {
                    self.transcript_cells
                        .insert(thread_id, SessionTranscriptState::Failed);
                    if self.pending_transcript_open == Some(thread_id) {
                        self.pending_transcript_open = None;
                        self.transcript_loading_frame_shown = false;
                        self.inline_error = Some("Could not load transcript preview".to_string());
                    }
                    self.request_frame();
                }
            },
        }
        Ok(())
    }

    /// Replaces the current result set with a new first page while preserving
    /// the selected thread when it is still present.
    fn replace_with_page(&mut self, page: PickerPage) {
        let selected_row = self.filtered_rows.get(self.selected);
        let selected_thread_id = selected_row.and_then(|row| row.thread_id);
        let selected_key = selected_row.and_then(Row::seen_key);
        let selected_index = self.selected;

        self.pagination.next_cursor = page.next_cursor;
        self.pagination.num_scanned_files = page.num_scanned_files;
        self.pagination.reached_scan_cap = page.reached_scan_cap;
        self.frozen_footer_percent = None;
        self.all_rows.clear();
        self.filtered_rows.clear();
        self.seen_rows.clear();

        for row in page.rows {
            if let Some(seen_key) = row.seen_key() {
                if self.seen_rows.insert(seen_key) {
                    self.all_rows.push(row);
                }
            } else {
                self.all_rows.push(row);
            }
        }

        self.apply_filter();
        self.selected = selected_thread_id
            .and_then(|selected_thread_id| {
                self.filtered_rows
                    .iter()
                    .position(|row| row.thread_id == Some(selected_thread_id))
            })
            .or_else(|| {
                selected_key.and_then(|selected_key| {
                    self.filtered_rows
                        .iter()
                        .position(|row| row.seen_key().as_ref() == Some(&selected_key))
                })
            })
            .unwrap_or_else(|| selected_index.min(self.filtered_rows.len().saturating_sub(1)));
        self.ensure_selected_visible();
        self.request_frame();
    }
}

#[cfg(test)]
#[path = "loading_tests.rs"]
mod tests;
