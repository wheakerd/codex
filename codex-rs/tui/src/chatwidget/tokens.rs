use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::RwLock;

use chrono::Datelike;
use chrono::Duration;
use chrono::NaiveDate;
use chrono::Utc;
use codex_app_server_protocol::GetAccountTokenUsageResponse;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;

use super::ChatWidget;
use crate::app_event::AppEvent;
use crate::color::blend;
use crate::history_cell::CompositeHistoryCell;
use crate::history_cell::HistoryCell;
use crate::history_cell::PlainHistoryCell;
use crate::history_cell::plain_lines;
use crate::render::highlight::foreground_style_for_scopes;
use crate::status::format_tokens_compact;
use crate::terminal_palette::StdoutColorLevel;
use crate::terminal_palette::best_color;
use crate::terminal_palette::default_bg;
use crate::terminal_palette::default_fg;
use crate::terminal_palette::stdout_color_level;

const EMPTY_CELL_GLYPH: &str = "∎";
const ACTIVE_CELL_GLYPH: &str = "■";
const WEEK_COUNT: usize = 52;
const DAY_COUNT: usize = 7;
const CELL_COUNT: usize = WEEK_COUNT * DAY_COUNT;
const CHART_LEFT_WIDTH: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TokenActivityView {
    Daily,
    Weekly,
    Cumulative,
}

impl TokenActivityView {
    pub(super) fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "" | "day" | "daily" => Some(Self::Daily),
            "week" | "weekly" => Some(Self::Weekly),
            "cumulative" => Some(Self::Cumulative),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Daily => "Daily",
            Self::Weekly => "Weekly",
            Self::Cumulative => "Cumulative",
        }
    }
}

#[derive(Debug)]
enum TokenActivityState {
    Loading,
    Loaded(GetAccountTokenUsageResponse),
    Error,
}

#[derive(Clone, Debug)]
pub(super) struct TokenActivityHandle {
    state: Arc<RwLock<TokenActivityState>>,
}

impl TokenActivityHandle {
    pub(super) fn finish(&self, result: Result<GetAccountTokenUsageResponse, String>) {
        let state = match result {
            Ok(response) => TokenActivityState::Loaded(response),
            Err(_) => TokenActivityState::Error,
        };
        #[expect(clippy::expect_used)]
        let mut current = self.state.write().expect("token activity state poisoned");
        *current = state;
    }
}

#[derive(Debug)]
struct TokenActivityHistoryCell {
    view: TokenActivityView,
    state: Arc<RwLock<TokenActivityState>>,
}

pub(super) fn new_token_activity_output(
    view: TokenActivityView,
) -> (CompositeHistoryCell, TokenActivityHandle) {
    let command = PlainHistoryCell::new(vec![
        format!("/tokens {}", view.label().to_lowercase())
            .magenta()
            .into(),
    ]);
    let state = Arc::new(RwLock::new(TokenActivityState::Loading));
    let handle = TokenActivityHandle {
        state: Arc::clone(&state),
    };
    let card = TokenActivityHistoryCell { view, state };
    (
        CompositeHistoryCell::new(vec![Box::new(command), Box::new(card)]),
        handle,
    )
}

impl HistoryCell for TokenActivityHistoryCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        #[expect(clippy::expect_used)]
        let state = self.state.read().expect("token activity state poisoned");
        match &*state {
            TokenActivityState::Loading => {
                vec![
                    " Token activity".bold().into(),
                    "   Loading...".dim().into(),
                ]
            }
            TokenActivityState::Error => vec![
                " Token activity".bold().into(),
                "   Token activity unavailable".dim().into(),
            ],
            TokenActivityState::Loaded(response) => self.loaded_lines(response, width),
        }
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        plain_lines(self.display_lines(u16::MAX))
    }
}

impl TokenActivityHistoryCell {
    fn loaded_lines(
        &self,
        response: &GetAccountTokenUsageResponse,
        width: u16,
    ) -> Vec<Line<'static>> {
        let mut lines = vec![" Token activity".bold().into()];
        lines.extend(summary_lines(response, width));
        let Some(buckets) = response.daily_usage_buckets.as_ref() else {
            lines.push("   Token activity history unavailable".dim().into());
            return lines;
        };

        lines.extend(self.chart_lines(buckets, Utc::now().date_naive(), width));
        lines
    }

    fn chart_lines(
        &self,
        buckets: &[codex_app_server_protocol::AccountTokenUsageDailyBucket],
        today: NaiveDate,
        width: u16,
    ) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let values = daily_values(buckets, today);
        let available_columns = usize::from(width)
            .saturating_sub(CHART_LEFT_WIDTH)
            .saturating_add(1)
            / 2;
        let shown_columns = available_columns.min(WEEK_COUNT);
        if shown_columns == 0 {
            lines.push("   Widen terminal to show activity graph".dim().into());
            return lines;
        }

        let palette = TokenActivityPalette::current();
        let levels = levels_for_view(&values, self.view);
        let first_column = WEEK_COUNT - shown_columns;
        lines.push(month_labels(today, first_column, shown_columns));
        for row in 0..DAY_COUNT {
            let mut spans = vec![weekday_label(self.view, row)];
            for column in first_column..WEEK_COUNT {
                if column > first_column {
                    spans.push(" ".into());
                }
                let index = column * DAY_COUNT + row;
                if self.view == TokenActivityView::Daily
                    && cell_date(today, index).is_some_and(|date| date > today)
                {
                    spans.push(" ".into());
                } else {
                    let style = if self.view == TokenActivityView::Daily {
                        palette.for_level(levels[index])
                    } else {
                        palette.for_bar_level(levels[index])
                    };
                    spans.push(Span::styled(cell_glyph(levels[index]), style));
                }
            }
            lines.push(spans.into());
        }
        lines.push(legend_line(&palette));
        lines
    }
}

fn summary_lines(response: &GetAccountTokenUsageResponse, width: u16) -> Vec<Line<'static>> {
    let summary = &response.summary;
    let fields = [
        ("Lifetime", format_optional_tokens(summary.lifetime_tokens)),
        ("Peak", format_optional_tokens(summary.peak_daily_tokens)),
        (
            "Current streak",
            format_optional_days(summary.current_streak_days),
        ),
        (
            "Longest streak",
            format_optional_days(summary.longest_streak_days),
        ),
        (
            "Longest turn",
            format_optional_duration(summary.longest_running_turn_sec),
        ),
    ];
    let line = summary_line(&fields, &[0, 1, 2, 3, 4]);
    if line.width() <= usize::from(width) || width == u16::MAX {
        return vec![center_summary_line(line, width)];
    }
    vec![
        center_summary_line(summary_line(&fields, &[0, 1, 4]), width),
        center_summary_line(summary_line(&fields, &[2, 3]), width),
    ]
}

fn summary_line(fields: &[(&str, String)], indexes: &[usize]) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, field_index) in indexes.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" · ", label_style()));
        }
        let (label, value) = &fields[*field_index];
        spans.push(Span::styled(format!("{label} "), label_style()));
        spans.push(Span::styled(value.clone(), numeric_style()));
    }
    spans.into()
}

fn center_summary_line(mut line: Line<'static>, width: u16) -> Line<'static> {
    if width == u16::MAX {
        return line;
    }
    let padding = usize::from(width).saturating_sub(line.width()) / 2;
    if padding > 0 {
        line.spans.insert(/*index*/ 0, " ".repeat(padding).into());
    }
    line
}

fn format_optional_tokens(value: Option<i64>) -> String {
    value
        .map(format_tokens_compact)
        .unwrap_or_else(|| "-".to_string())
}

fn format_optional_days(value: Option<i64>) -> String {
    value.map_or_else(|| "-".to_string(), |days| format!("{days}d"))
}

fn format_optional_duration(value: Option<i64>) -> String {
    value.map_or_else(
        || "-".to_string(),
        |seconds| {
            let minutes = seconds / 60;
            if minutes > 0 {
                format!("{minutes}m")
            } else {
                format!("{seconds}s")
            }
        },
    )
}

fn numeric_style() -> Style {
    foreground_style_for_scopes(&["constant.numeric", "constant"])
        .unwrap_or_else(|| Style::default().green())
}

fn label_style() -> Style {
    foreground_style_for_scopes(&["comment"]).unwrap_or_else(|| Style::default().dim())
}

fn weekday_label(view: TokenActivityView, row: usize) -> Span<'static> {
    if view != TokenActivityView::Daily {
        return "    ".into();
    }
    Span::styled(
        match row {
            0 => " Su ",
            1 => " Mo ",
            2 => " Tu ",
            3 => " We ",
            4 => " Th ",
            5 => " Fr ",
            6 => " Sa ",
            _ => "    ",
        },
        label_style(),
    )
}

fn legend_line(palette: &TokenActivityPalette) -> Line<'static> {
    let mut spans = vec![Span::styled("   Less ", label_style())];
    for level in 0..=4 {
        if level > 0 {
            spans.push(" ".into());
        }
        spans.push(Span::styled(ACTIVE_CELL_GLYPH, palette.for_level(level)));
    }
    spans.push(Span::styled(" More", label_style()));
    spans.into()
}

fn cell_glyph(level: usize) -> &'static str {
    if level == 0 {
        EMPTY_CELL_GLYPH
    } else {
        ACTIVE_CELL_GLYPH
    }
}

fn month_labels(today: NaiveDate, first_column: usize, shown_columns: usize) -> Line<'static> {
    let mut cells = vec![' '; shown_columns * 2 - 1];
    let start = chart_start(today);
    let mut last_end = 0;
    for column in first_column..WEEK_COUNT {
        let date = start + Duration::days((column * DAY_COUNT) as i64);
        if date.day() > 7 {
            continue;
        }
        let label = date.format("%b").to_string();
        let offset = (column - first_column) * 2;
        if offset < last_end || offset + label.len() > cells.len() {
            continue;
        }
        for (index, ch) in label.chars().enumerate() {
            cells[offset + index] = ch;
        }
        last_end = offset + label.len() + 1;
    }
    vec![
        "    ".into(),
        Span::styled(cells.into_iter().collect::<String>(), label_style()),
    ]
    .into()
}

fn daily_values(
    buckets: &[codex_app_server_protocol::AccountTokenUsageDailyBucket],
    today: NaiveDate,
) -> Vec<i64> {
    let start = chart_start(today);
    let end = start + Duration::days(CELL_COUNT as i64);
    let mut by_date = BTreeMap::new();
    for bucket in buckets {
        let Ok(date) = NaiveDate::parse_from_str(&bucket.start_date, "%Y-%m-%d") else {
            continue;
        };
        if date < start || date >= end || date > today {
            continue;
        }
        *by_date.entry(date).or_insert(0) += bucket.tokens.max(0);
    }
    (0..CELL_COUNT)
        .map(|offset| {
            by_date
                .get(&(start + Duration::days(offset as i64)))
                .copied()
                .unwrap_or(0)
        })
        .collect()
}

fn levels_for_view(values: &[i64], view: TokenActivityView) -> Vec<usize> {
    match view {
        TokenActivityView::Daily => graded_levels(values),
        TokenActivityView::Weekly => bar_levels(&weekly_totals(values)),
        TokenActivityView::Cumulative => {
            let cumulative = weekly_totals(values)
                .into_iter()
                .scan(0, |sum, value| {
                    *sum += value;
                    Some(*sum)
                })
                .collect::<Vec<_>>();
            bar_levels(&cumulative)
        }
    }
}

fn graded_levels(values: &[i64]) -> Vec<usize> {
    let max = values.iter().copied().max().unwrap_or(0);
    values
        .iter()
        .map(|value| match (*value, max) {
            (0, _) | (_, 0) => 0,
            (value, max) if value * 4 > max * 3 => 4,
            (value, max) if value * 2 > max => 3,
            (value, max) if value * 4 > max => 2,
            _ => 1,
        })
        .collect()
}

fn weekly_totals(values: &[i64]) -> Vec<i64> {
    values
        .chunks(DAY_COUNT)
        .map(|week| week.iter().sum())
        .collect()
}

fn bar_levels(totals: &[i64]) -> Vec<usize> {
    let max = totals.iter().copied().max().unwrap_or(0);
    totals
        .iter()
        .flat_map(|value| {
            let height = if *value <= 0 || max <= 0 {
                0
            } else {
                ((*value * DAY_COUNT as i64 + max - 1) / max) as usize
            };
            (0..DAY_COUNT).map(move |row| if DAY_COUNT - row <= height { 4 } else { 0 })
        })
        .collect()
}

fn chart_start(today: NaiveDate) -> NaiveDate {
    let week_start = today - Duration::days(i64::from(today.weekday().num_days_from_sunday()));
    week_start - Duration::weeks((WEEK_COUNT - 1) as i64)
}

fn cell_date(today: NaiveDate, index: usize) -> Option<NaiveDate> {
    chart_start(today).checked_add_signed(Duration::days(index as i64))
}

struct TokenActivityPalette {
    styles: [Style; 5],
    bar_style: Style,
}

impl TokenActivityPalette {
    fn current() -> Self {
        let fallback = [
            Style::default().dim(),
            Style::default().green().dim(),
            Style::default().green(),
            Style::default().light_green(),
            Style::default().light_green().bold(),
        ];
        let fallback_bar_style = Style::default().light_green();
        let (Some(fg), Some(bg), Some(anchor)) = (default_fg(), default_bg(), theme_anchor_rgb())
        else {
            return Self {
                styles: fallback,
                bar_style: fallback_bar_style,
            };
        };
        if matches!(
            stdout_color_level(),
            StdoutColorLevel::Ansi16 | StdoutColorLevel::Unknown
        ) {
            return Self {
                styles: fallback,
                bar_style: fallback_bar_style,
            };
        }
        let empty_alpha = if crate::color::is_light(bg) {
            0.18
        } else {
            0.10
        };
        let alphas = [empty_alpha, 0.22, 0.42, 0.68, 1.00];
        let styles = std::array::from_fn(|index| {
            let color = if index == 0 {
                blend(fg, bg, alphas[index])
            } else {
                blend(anchor, bg, alphas[index])
            };
            Style::default().fg(best_color(color))
        });
        let bar_style = Style::default().fg(best_color(blend(anchor, bg, 0.78)));
        Self { styles, bar_style }
    }

    fn for_level(&self, level: usize) -> Style {
        self.styles[level.min(4)]
    }

    fn for_bar_level(&self, level: usize) -> Style {
        if level == 0 {
            self.for_level(0)
        } else {
            self.bar_style
        }
    }
}

fn theme_anchor_rgb() -> Option<(u8, u8, u8)> {
    match numeric_style().fg? {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        _ => None,
    }
}

impl ChatWidget {
    pub(super) fn add_token_activity_output(&mut self, view: TokenActivityView) {
        let request_id = self.next_token_activity_request_id;
        self.next_token_activity_request_id = self.next_token_activity_request_id.wrapping_add(1);
        let (cell, handle) = new_token_activity_output(view);
        self.refreshing_token_activity_outputs
            .push((request_id, handle));
        self.add_to_history(cell);
        self.app_event_tx
            .send(AppEvent::RefreshTokenActivity { request_id });
    }

    pub(crate) fn finish_token_activity_refresh(
        &mut self,
        request_id: u64,
        result: Result<GetAccountTokenUsageResponse, String>,
    ) -> bool {
        let mut remaining = Vec::with_capacity(self.refreshing_token_activity_outputs.len());
        let mut result = Some(result);
        let mut updated_any = false;
        for (pending_request_id, handle) in self.refreshing_token_activity_outputs.drain(..) {
            if pending_request_id == request_id {
                #[expect(clippy::expect_used)]
                handle.finish(result.take().expect("token activity result consumed once"));
                updated_any = true;
            } else {
                remaining.push((pending_request_id, handle));
            }
        }
        self.refreshing_token_activity_outputs = remaining;
        if updated_any {
            self.request_redraw();
        }
        updated_any
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_app_server_protocol::AccountTokenUsageDailyBucket;
    use codex_app_server_protocol::AccountTokenUsageSummary;
    use insta::assert_snapshot;
    use pretty_assertions::assert_eq;

    #[test]
    fn duplicate_dates_sum_and_negative_values_clamp() {
        let today = NaiveDate::from_ymd_opt(2026, 5, 29).expect("valid date");
        let buckets = vec![
            AccountTokenUsageDailyBucket {
                start_date: "2026-05-29".to_string(),
                tokens: 10,
            },
            AccountTokenUsageDailyBucket {
                start_date: "2026-05-29".to_string(),
                tokens: 5,
            },
            AccountTokenUsageDailyBucket {
                start_date: "2026-05-28".to_string(),
                tokens: -4,
            },
        ];

        let values = daily_values(&buckets, today);

        assert_eq!(values.iter().sum::<i64>(), 15);
    }

    #[test]
    fn bar_levels_fill_from_bottom() {
        let levels = bar_levels(&[0, 10]);

        assert_eq!(&levels[..DAY_COUNT], &[0; DAY_COUNT]);
        assert_eq!(&levels[DAY_COUNT..], &[4; DAY_COUNT]);
    }

    #[test]
    fn token_activity_view_aliases_parse() {
        assert_eq!(TokenActivityView::parse(""), Some(TokenActivityView::Daily));
        assert_eq!(
            TokenActivityView::parse("day"),
            Some(TokenActivityView::Daily)
        );
        assert_eq!(
            TokenActivityView::parse("week"),
            Some(TokenActivityView::Weekly)
        );
        assert_eq!(
            TokenActivityView::parse("cumulative"),
            Some(TokenActivityView::Cumulative)
        );
        assert_eq!(TokenActivityView::parse("year"), None);
    }

    #[test]
    fn daily_graph_snapshot_uses_distinct_empty_and_active_cells() {
        let today = NaiveDate::from_ymd_opt(2026, 5, 29).expect("valid date");
        let buckets = vec![
            AccountTokenUsageDailyBucket {
                start_date: "2026-05-25".to_string(),
                tokens: 1,
            },
            AccountTokenUsageDailyBucket {
                start_date: "2026-05-29".to_string(),
                tokens: 4,
            },
        ];
        let cell = TokenActivityHistoryCell {
            view: TokenActivityView::Daily,
            state: Arc::new(RwLock::new(TokenActivityState::Loading)),
        };

        let rendered = cell
            .chart_lines(&buckets, today, /*width*/ 22)
            .into_iter()
            .map(|line| line.to_string().trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n");

        assert_snapshot!(rendered, @r"
             Apr     May
        Su ∎ ∎ ∎ ∎ ∎ ∎ ∎ ∎ ∎
        Mo ∎ ∎ ∎ ∎ ∎ ∎ ∎ ∎ ■
        Tu ∎ ∎ ∎ ∎ ∎ ∎ ∎ ∎ ∎
        We ∎ ∎ ∎ ∎ ∎ ∎ ∎ ∎ ∎
        Th ∎ ∎ ∎ ∎ ∎ ∎ ∎ ∎ ∎
        Fr ∎ ∎ ∎ ∎ ∎ ∎ ∎ ∎ ■
        Sa ∎ ∎ ∎ ∎ ∎ ∎ ∎ ∎
          Less ■ ■ ■ ■ ■ More
        ");
    }

    #[test]
    fn summary_snapshot_centers_and_splits_when_needed() {
        let response = GetAccountTokenUsageResponse {
            summary: AccountTokenUsageSummary {
                lifetime_tokens: Some(21_400_000_000),
                peak_daily_tokens: Some(835_000_000),
                longest_running_turn_sec: Some(13_920),
                current_streak_days: Some(54),
                longest_streak_days: Some(54),
            },
            daily_usage_buckets: None,
        };
        let rendered = |width| {
            summary_lines(&response, width)
                .into_iter()
                .map(|line| line.to_string().trim_end().to_string())
                .collect::<Vec<_>>()
                .join("\n")
        };

        assert_snapshot!(
            format!(
                "wide:\n{}\n\nnarrow:\n{}",
                rendered(/*width*/ 120),
                rendered(/*width*/ 80)
            ),
            @"
        wide:
                        Lifetime 21.4B · Peak 835M · Current streak 54d · Longest streak 54d · Longest turn 232m

        narrow:
                         Lifetime 21.4B · Peak 835M · Longest turn 232m
                            Current streak 54d · Longest streak 54d
        "
        );
    }
}
