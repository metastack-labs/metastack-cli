use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{
    Arc, RwLock,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result};

use super::{ListenDashboardData, SessionListView};

pub struct ListenDashboardServer {
    url: String,
    shutdown: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl ListenDashboardServer {
    pub fn start(host: &str, port: u16, state: Arc<RwLock<ListenDashboardData>>) -> Result<Self> {
        let listener = TcpListener::bind((host, port))
            .with_context(|| format!("failed to bind local dashboard to {host}:{port}"))?;
        listener
            .set_nonblocking(true)
            .context("failed to configure the local dashboard listener")?;
        let address = listener
            .local_addr()
            .context("failed to determine the local dashboard address")?;
        let url = format!("http://{host}:{}/", address.port());
        let shutdown = Arc::new(AtomicBool::new(false));
        let handle = {
            let shutdown = shutdown.clone();
            thread::Builder::new()
                .name("meta-listen-dashboard".to_string())
                .spawn(move || serve(listener, state, shutdown))
                .context("failed to spawn the local dashboard server thread")?
        };

        Ok(Self {
            url,
            shutdown,
            handle: Some(handle),
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }
}

impl Drop for ListenDashboardServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn serve(
    listener: TcpListener,
    state: Arc<RwLock<ListenDashboardData>>,
    shutdown: Arc<AtomicBool>,
) {
    while !shutdown.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                let _ = handle_connection(stream, &state);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(75));
            }
            Err(_) => break,
        }
    }
}

fn handle_connection(
    mut stream: TcpStream,
    state: &Arc<RwLock<ListenDashboardData>>,
) -> Result<()> {
    let mut buffer = [0u8; 4096];
    let size = stream
        .read(&mut buffer)
        .context("failed to read the local dashboard request")?;
    let request = String::from_utf8_lossy(&buffer[..size]);
    let first_line = request.lines().next().unwrap_or_default();
    let target = first_line.split_whitespace().nth(1).unwrap_or("/");
    let (path, query) = target
        .split_once('?')
        .map_or((target, ""), |(path, query)| (path, query));
    let is_head = first_line.starts_with("HEAD ");

    let (status, content_type, body) = match path {
        "/health" => ("200 OK", "text/plain; charset=utf-8", "ok".to_string()),
        _ => {
            let data = state
                .read()
                .map(|guard| guard.clone())
                .unwrap_or_else(|_| fallback_dashboard());
            let view = SessionListView::from_query(query);
            (
                "200 OK",
                "text/html; charset=utf-8",
                if view == SessionListView::Active {
                    render_html(&data)
                } else {
                    render_html_with_view(&data, view)
                },
            )
        }
    };

    let mut response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    if !is_head {
        response.push_str(&body);
    }

    stream
        .write_all(response.as_bytes())
        .context("failed to write the local dashboard response")?;
    stream
        .flush()
        .context("failed to flush the local dashboard response")
}

pub fn render_html(data: &ListenDashboardData) -> String {
    render_html_with_view(data, SessionListView::Active)
}

fn render_html_with_view(data: &ListenDashboardData, view: SessionListView) -> String {
    let refresh_seconds = data.runtime.dashboard_refresh_seconds.max(1);
    let counts = data.session_counts();
    let sessions = data.sessions_for_view(view);
    let mut html = String::new();

    html.push_str("<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\">");
    html.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">");
    html.push_str(&format!(
        "<meta http-equiv=\"refresh\" content=\"{refresh_seconds}\">"
    ));
    html.push_str("<title>meta listen dashboard</title><style>");
    html.push_str(STYLESHEET);
    html.push_str(
        "</style></head><body><main class=\"app-shell\"><section class=\"dashboard-shell\">",
    );

    html.push_str("<header class=\"hero-card\"><div class=\"hero-grid\"><div>");
    html.push_str("<p class=\"eyebrow\">meta listen</p>");
    html.push_str(&format!(
        "<h1 class=\"hero-title\">{}</h1>",
        escape_html(&data.scope)
    ));
    html.push_str(&format!(
        "<p class=\"hero-copy\">{}</p>",
        escape_html(&data.cycle_summary)
    ));
    html.push_str("</div><div class=\"status-stack\">");
    html.push_str("<span class=\"status-badge status-badge-live\"><span class=\"status-badge-dot\"></span>Live</span>");
    html.push_str("</div></div>");
    html.push_str(&format!(
        "<p class=\"hero-meta mono\">State file: {}</p>",
        escape_html(&data.state_file)
    ));
    html.push_str("</header>");

    html.push_str("<section class=\"metric-grid\">");
    metric_card(
        &mut html,
        "Agents",
        &data.runtime.agents,
        "Tracked sessions and queued Todo work.",
    );
    metric_card(
        &mut html,
        "Throughput",
        &data.runtime.throughput,
        "Issues claimed per second on the current cadence.",
    );
    metric_card(
        &mut html,
        "Runtime",
        &data.runtime.runtime,
        "Total runtime for the current listen session.",
    );
    metric_card(
        &mut html,
        "Tokens",
        &data.runtime.tokens,
        "Aggregated token counts when agent telemetry is available.",
    );
    html.push_str("</section>");

    html.push_str("<section class=\"section-card\"><div class=\"section-header\"><div>");
    html.push_str("<h2 class=\"section-title\">Runtime summary</h2>");
    html.push_str(
        "<p class=\"section-copy\">Current daemon cadence, scope, and dashboard endpoints.</p>",
    );
    html.push_str("</div></div><dl class=\"detail-grid\">");
    detail_row(&mut html, "Rate Limits", &data.runtime.rate_limits);
    detail_row(&mut html, "Project", &data.runtime.project);
    detail_dashboard_row(
        &mut html,
        "Dashboard",
        data.runtime.dashboard_url.as_deref(),
        &data.runtime.dashboard,
    );
    detail_row(
        &mut html,
        "Dashboard refresh",
        &data.runtime.dashboard_refresh,
    );
    detail_row(&mut html, "Linear refresh", &data.runtime.linear_refresh);
    html.push_str("</dl></section>");

    html.push_str("<section class=\"section-card\"><div class=\"section-header\"><div>");
    html.push_str("<h2 class=\"section-title\">Agent sessions</h2>");
    html.push_str("<p class=\"section-copy\">Active issues, local session handles, and compact backlog-driven progress.</p>");
    html.push_str("</div><nav class=\"segmented-control\" aria-label=\"Session view\">");
    session_toggle_link(&mut html, SessionListView::Active, view, counts.active);
    session_toggle_link(
        &mut html,
        SessionListView::Completed,
        view,
        counts.completed,
    );
    html.push_str("</nav></div>");

    if sessions.is_empty() {
        html.push_str(&format!(
            "<p class=\"empty-state\">{}</p>",
            match view {
                SessionListView::Active => "No active agent sessions are currently tracked.",
                SessionListView::Completed => "No completed agent sessions are currently tracked.",
            }
        ));
    } else {
        html.push_str("<div class=\"table-wrap\"><table class=\"data-table\"><thead><tr>");
        for heading in ["ID", "Stage", "PID", "Age", "Tokens", "Session", "Progress"] {
            html.push_str(&format!("<th>{heading}</th>"));
        }
        html.push_str("</tr></thead><tbody>");

        for session in sessions {
            html.push_str("<tr>");
            html.push_str("<td><div class=\"issue-stack\">");
            html.push_str(&format!(
                "<span class=\"issue-id\">{}</span><span class=\"muted\">{}</span>",
                escape_html(&session.issue_identifier),
                escape_html(&session.issue_title),
            ));
            if let Some(backlog_issue_identifier) = session.backlog_issue_identifier.as_deref()
                && !backlog_issue_identifier.eq_ignore_ascii_case(&session.issue_identifier)
            {
                html.push_str(&format!(
                    "<span class=\"muted\">backlog {}</span>",
                    escape_html(backlog_issue_identifier),
                ));
            }
            html.push_str("</div></td>");
            html.push_str(&format!(
                "<td><span class=\"state-badge state-badge-{}\">{}</span></td>",
                session.phase.html_class(),
                escape_html(session.stage_label()),
            ));
            html.push_str(&format!(
                "<td class=\"mono\">{}</td>",
                escape_html(&session.pid_label())
            ));
            html.push_str(&format!(
                "<td class=\"mono\">{}</td>",
                escape_html(&session.age_label(data.runtime.current_epoch_seconds))
            ));
            html.push_str(&format!(
                "<td class=\"mono\">{}</td>",
                escape_html(&session.tokens_label())
            ));
            html.push_str(&format!(
                "<td class=\"mono\">{}</td>",
                escape_html(&session.session_label())
            ));
            html.push_str(&format!(
                "<td><span class=\"event-text\" title=\"{}\">{}</span></td>",
                escape_html(&session.summary),
                escape_html(&session.summary),
            ));
            html.push_str("</tr>");
        }

        html.push_str("</tbody></table></div>");
    }
    html.push_str("</section>");

    html.push_str("<section class=\"section-grid\">");
    html.push_str("<section class=\"section-card\"><div class=\"section-header\"><div>");
    html.push_str("<h2 class=\"section-title\">Todo queue</h2>");
    html.push_str(
        "<p class=\"section-copy\">Tickets still waiting to be picked up by the listener.</p>",
    );
    html.push_str("</div></div>");
    if data.pending_issues.is_empty() {
        html.push_str("<p class=\"empty-state\">No queued Todo tickets.</p>");
    } else {
        html.push_str("<ul class=\"stack-list\">");
        for issue in &data.pending_issues {
            html.push_str("<li>");
            html.push_str(&format!(
                "<strong>{}</strong> <span class=\"muted\">[{}]</span><br><span class=\"muted\">{} · {}</span>",
                escape_html(&issue.identifier),
                escape_html(&issue.team_key),
                escape_html(issue.project.as_deref().unwrap_or("No project")),
                escape_html(&issue.title),
            ));
            html.push_str("</li>");
        }
        html.push_str("</ul>");
    }
    html.push_str("</section>");

    html.push_str("<section class=\"section-card\"><div class=\"section-header\"><div>");
    html.push_str("<h2 class=\"section-title\">Notes</h2>");
    html.push_str(
        "<p class=\"section-copy\">Latest daemon observations from the current cycle.</p>",
    );
    html.push_str("</div></div>");
    if data.notes.is_empty() {
        html.push_str("<p class=\"empty-state\">No daemon notes were recorded for this cycle.</p>");
    } else {
        html.push_str("<ul class=\"stack-list\">");
        for note in &data.notes {
            html.push_str(&format!("<li>{}</li>", escape_html(note)));
        }
        html.push_str("</ul>");
    }
    html.push_str("</section></section></section></main></body></html>");

    html
}

fn session_toggle_link(
    html: &mut String,
    candidate: SessionListView,
    active_view: SessionListView,
    count: usize,
) {
    let is_active = candidate == active_view;
    html.push_str(&format!(
        "<a class=\"toggle-link{}\" href=\"/?view={}\"{}>{} <span class=\"toggle-count\">{count}</span></a>",
        if is_active { " toggle-link-active" } else { "" },
        candidate.query_value(),
        if is_active {
            " aria-current=\"page\""
        } else {
            ""
        },
        escape_html(candidate.label()),
    ));
}

fn metric_card(html: &mut String, label: &str, value: &str, detail: &str) {
    html.push_str("<article class=\"metric-card\">");
    html.push_str(&format!(
        "<p class=\"metric-label\">{}</p><p class=\"metric-value mono\">{}</p><p class=\"metric-detail\">{}</p>",
        escape_html(label),
        escape_html(value),
        escape_html(detail),
    ));
    html.push_str("</article>");
}

fn detail_row(html: &mut String, label: &str, value: &str) {
    html.push_str("<div class=\"detail-row\">");
    html.push_str(&format!(
        "<dt>{}</dt><dd>{}</dd>",
        escape_html(label),
        escape_html(value),
    ));
    html.push_str("</div>");
}

fn detail_dashboard_row(html: &mut String, label: &str, url: Option<&str>, value: &str) {
    html.push_str("<div class=\"detail-row\">");
    html.push_str(&format!("<dt>{}</dt>", escape_html(label)));
    if let Some(url) = url {
        html.push_str(&format!(
            "<dd><a href=\"{}\">{}</a></dd>",
            escape_html(url),
            escape_html(value),
        ));
    } else {
        html.push_str(&format!("<dd>{}</dd>", escape_html(value)));
    }
    html.push_str("</div>");
}

fn fallback_dashboard() -> ListenDashboardData {
    ListenDashboardData {
        title: "meta listen".to_string(),
        scope: "dashboard unavailable".to_string(),
        cycle_summary: "The local dashboard state could not be loaded.".to_string(),
        runtime: crate::listen::ListenRuntimeSummary {
            agents: "n/a".to_string(),
            throughput: "n/a".to_string(),
            runtime: "n/a".to_string(),
            tokens: "n/a".to_string(),
            rate_limits: "n/a".to_string(),
            project: "n/a".to_string(),
            dashboard: "n/a".to_string(),
            dashboard_url: None,
            dashboard_refresh: "1s".to_string(),
            dashboard_refresh_seconds: 1,
            linear_refresh: "n/a".to_string(),
            current_epoch_seconds: 0,
        },
        pending_issues: Vec::new(),
        sessions: Vec::new(),
        notes: vec!["Dashboard state lock was poisoned.".to_string()],
        state_file: "n/a".to_string(),
    }
}

fn escape_html(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

const STYLESHEET: &str = r#"
:root {
  color-scheme: light;
  --page: #f7f7f8;
  --page-soft: #fbfbfc;
  --page-deep: #ececf1;
  --card: rgba(255, 255, 255, 0.94);
  --card-muted: #f3f4f6;
  --ink: #202123;
  --muted: #6e6e80;
  --line: #ececf1;
  --line-strong: #d9d9e3;
  --accent: #10a37f;
  --accent-ink: #0f513f;
  --accent-soft: #e8faf4;
  --warning: #8a5a00;
  --warning-soft: #fff7e8;
  --danger: #b42318;
  --danger-soft: #fef3f2;
  --shadow-sm: 0 1px 2px rgba(16, 24, 40, 0.05);
  --shadow-lg: 0 20px 50px rgba(15, 23, 42, 0.08);
}

* { box-sizing: border-box; }
html { background: var(--page); }
body {
  margin: 0;
  min-height: 100vh;
  background:
    radial-gradient(circle at top, rgba(16, 163, 127, 0.12) 0%, rgba(16, 163, 127, 0) 30%),
    linear-gradient(180deg, var(--page-soft) 0%, var(--page) 24%, #f3f4f6 100%);
  color: var(--ink);
  font-family: "Sohne", "SF Pro Text", "Helvetica Neue", "Segoe UI", sans-serif;
  line-height: 1.5;
}

a {
  color: var(--ink);
  text-decoration: none;
}

a:hover { color: var(--accent); }

code,
.mono {
  font-family: "Sohne Mono", "SFMono-Regular", "SF Mono", Consolas, "Liberation Mono", monospace;
}

.mono {
  font-variant-numeric: tabular-nums slashed-zero;
  font-feature-settings: "tnum" 1, "zero" 1;
}

.app-shell {
  max-width: 1400px;
  margin: 0 auto;
  padding: 2rem 1rem 3rem;
}

.dashboard-shell {
  display: grid;
  gap: 1rem;
}

.hero-card,
.section-card,
.metric-card {
  background: var(--card);
  border: 1px solid rgba(217, 217, 227, 0.82);
  box-shadow: var(--shadow-sm);
  backdrop-filter: blur(18px);
}

.hero-card {
  border-radius: 28px;
  padding: clamp(1.25rem, 3vw, 2rem);
  box-shadow: var(--shadow-lg);
}

.hero-grid {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto;
  gap: 1.25rem;
  align-items: start;
}

.eyebrow {
  margin: 0;
  color: var(--muted);
  text-transform: uppercase;
  letter-spacing: 0.08em;
  font-size: 0.76rem;
  font-weight: 600;
}

.hero-title {
  margin: 0.35rem 0 0;
  font-size: clamp(2rem, 4vw, 3.2rem);
  line-height: 0.98;
  letter-spacing: -0.04em;
}

.hero-copy,
.hero-meta {
  margin: 0.75rem 0 0;
  color: var(--muted);
}

.status-stack {
  display: grid;
  justify-items: end;
}

.status-badge {
  display: inline-flex;
  align-items: center;
  gap: 0.45rem;
  min-height: 2rem;
  padding: 0.35rem 0.78rem;
  border-radius: 999px;
  border: 1px solid rgba(16, 163, 127, 0.18);
  background: var(--accent-soft);
  color: var(--accent-ink);
  font-size: 0.82rem;
  font-weight: 700;
}

.status-badge-dot {
  width: 0.52rem;
  height: 0.52rem;
  border-radius: 999px;
  background: currentColor;
}

.metric-grid {
  display: grid;
  gap: 0.85rem;
  grid-template-columns: repeat(auto-fit, minmax(200px, 1fr));
}

.metric-card,
.section-card {
  border-radius: 24px;
  padding: 1.1rem;
}

.metric-label,
.section-copy,
.muted,
.empty-state {
  color: var(--muted);
}

.metric-label {
  margin: 0;
  font-size: 0.82rem;
  font-weight: 600;
  letter-spacing: 0.01em;
}

.metric-value {
  margin: 0.35rem 0 0;
  font-size: clamp(1.4rem, 2vw, 2rem);
  line-height: 1.05;
  letter-spacing: -0.03em;
}

.metric-detail {
  margin: 0.45rem 0 0;
  font-size: 0.88rem;
}

.section-header {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  gap: 1rem;
  flex-wrap: wrap;
}

.section-title {
  margin: 0;
  font-size: 1.08rem;
  line-height: 1.2;
  letter-spacing: -0.02em;
}

.detail-grid {
  display: grid;
  gap: 0.75rem;
  grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
  margin: 1rem 0 0;
}

.segmented-control {
  display: inline-flex;
  align-items: center;
  gap: 0.45rem;
  padding: 0.28rem;
  border-radius: 999px;
  background: var(--page-deep);
}

.toggle-link {
  display: inline-flex;
  align-items: center;
  gap: 0.45rem;
  padding: 0.42rem 0.78rem;
  border-radius: 999px;
  color: var(--muted);
  font-size: 0.84rem;
  font-weight: 600;
  transition: background-color 120ms ease, color 120ms ease;
}

.toggle-link:hover {
  color: var(--ink);
  background: rgba(255, 255, 255, 0.6);
}

.toggle-link-active {
  background: var(--card);
  color: var(--ink);
  box-shadow: var(--shadow-sm);
}

.toggle-count {
  color: inherit;
  opacity: 0.78;
}

.detail-row {
  display: grid;
  gap: 0.18rem;
}

.detail-row dt {
  color: var(--muted);
  font-size: 0.8rem;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.04em;
}

.detail-row dd {
  margin: 0;
  color: var(--ink);
  font-size: 0.95rem;
}

.table-wrap {
  overflow-x: auto;
  margin-top: 1rem;
}

.data-table {
  width: 100%;
  min-width: 980px;
  border-collapse: collapse;
  table-layout: fixed;
}

.data-table th {
  padding: 0 0.5rem 0.75rem 0;
  text-align: left;
  color: var(--muted);
  font-size: 0.78rem;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.04em;
}

.data-table td {
  padding: 0.9rem 0.5rem 0.9rem 0;
  border-top: 1px solid var(--line);
  vertical-align: top;
  font-size: 0.94rem;
}

.issue-stack {
  display: grid;
  gap: 0.24rem;
  min-width: 0;
}

.issue-id {
  font-weight: 600;
  letter-spacing: -0.01em;
}

.event-text {
  display: block;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.state-badge {
  display: inline-flex;
  align-items: center;
  min-height: 1.85rem;
  padding: 0.3rem 0.68rem;
  border-radius: 999px;
  border: 1px solid var(--line);
  background: var(--card-muted);
  color: var(--ink);
  font-size: 0.8rem;
  font-weight: 600;
  line-height: 1;
}

.state-badge-active {
  background: var(--accent-soft);
  border-color: rgba(16, 163, 127, 0.18);
  color: var(--accent-ink);
}

.state-badge-warning {
  background: var(--warning-soft);
  border-color: #f1d8a6;
  color: var(--warning);
}

.state-badge-danger {
  background: var(--danger-soft);
  border-color: #f6d3cf;
  color: var(--danger);
}

.section-grid {
  display: grid;
  gap: 1rem;
  grid-template-columns: repeat(auto-fit, minmax(300px, 1fr));
}

.stack-list {
  margin: 1rem 0 0;
  padding-left: 1rem;
}

.stack-list li + li {
  margin-top: 0.65rem;
}

@media (max-width: 860px) {
  .app-shell {
    padding: 1rem 0.85rem 2rem;
  }

  .hero-grid {
    grid-template-columns: 1fr;
  }

  .status-stack {
    justify-items: start;
  }
}
"#;

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{render_html, render_html_with_view};
    use crate::listen::{
        DashboardRuntimeContext, ListenCycleData, SessionListView, SessionPhase,
        build_dashboard_data,
    };

    #[test]
    fn html_render_contains_runtime_summary_and_table() {
        let cycle = ListenCycleData::demo(Path::new("."));
        let data = build_dashboard_data(
            &cycle,
            &DashboardRuntimeContext {
                started_at_epoch_seconds: 1_773_568_249,
                now_epoch_seconds: 1_773_575_600,
                poll_interval_seconds: 7,
                dashboard_refresh_seconds: 1,
                linear_refresh_seconds: 12,
                dashboard_url: Some("http://127.0.0.1:4000/".to_string()),
            },
        );

        let html = render_html(&data);

        assert!(html.contains("meta listen dashboard"));
        assert!(html.contains("Runtime summary"));
        assert!(html.contains("Agent sessions"));
        assert!(html.contains("href=\"/?view=completed\""));
        assert!(html.contains("MET-13"));
        assert!(html.contains("Dashboard refresh"));
        assert!(html.contains("Linear refresh"));
        assert!(html.contains("<meta http-equiv=\"refresh\" content=\"1\">"));
    }

    #[test]
    fn html_refresh_uses_dashboard_cadence_not_linear_poll_interval() {
        let cycle = ListenCycleData::demo(Path::new("."));
        let data = build_dashboard_data(
            &cycle,
            &DashboardRuntimeContext {
                started_at_epoch_seconds: 1_773_568_249,
                now_epoch_seconds: 1_773_575_600,
                poll_interval_seconds: 42,
                dashboard_refresh_seconds: 1,
                linear_refresh_seconds: 42,
                dashboard_url: Some("http://127.0.0.1:4000/".to_string()),
            },
        );

        let html = render_html(&data);

        assert!(html.contains("<meta http-equiv=\"refresh\" content=\"1\">"));
        assert!(html.contains("<dt>Dashboard refresh</dt><dd>1s</dd>"));
        assert!(html.contains("<dt>Linear refresh</dt><dd>42s</dd>"));
        assert!(!html.contains("<meta http-equiv=\"refresh\" content=\"42\">"));
    }

    #[test]
    fn html_can_render_completed_session_view() {
        let mut cycle = ListenCycleData::demo(Path::new("."));
        let mut completed = cycle
            .sessions
            .first()
            .expect("demo cycle should include a session")
            .clone();
        completed.issue_identifier = "MET-99".to_string();
        completed.issue_title = "Completed ticket".to_string();
        completed.phase = SessionPhase::Completed;
        completed.summary = "Complete | moved to `Human Review`".to_string();
        cycle.sessions.push(completed);

        let data = build_dashboard_data(
            &cycle,
            &DashboardRuntimeContext {
                started_at_epoch_seconds: 1_773_568_249,
                now_epoch_seconds: 1_773_575_600,
                poll_interval_seconds: 7,
                dashboard_refresh_seconds: 1,
                linear_refresh_seconds: 12,
                dashboard_url: Some("http://127.0.0.1:4000/".to_string()),
            },
        );

        let html = render_html_with_view(&data, SessionListView::Completed);

        assert!(html.contains("MET-99"));
        assert!(!html.contains("MET-17"));
        assert!(html.contains("toggle-link-active"));
    }
}
