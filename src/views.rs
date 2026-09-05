use crate::{model::*, web::App};
use maud::{DOCTYPE, Markup, html};

fn video_icon() -> Markup {
    html! {svg width="64" height="54" viewBox="0 0 64 54" fill="none" aria-hidden="true" {rect x="2" y="2" width="60" height="50" rx="8" stroke="currentColor" stroke-width="2.5";path d="M26 17L43 27L26 37Z" stroke="currentColor" stroke-width="2.5" stroke-linejoin="round";}}
}
pub fn page(app: &App, title: &str, content: Markup) -> String {
    html! {(DOCTYPE) html lang="en" {head {meta charset="utf-8";meta name="viewport" content="width=device-width, initial-scale=1";meta name="csrf-token" content=(app.csrf);title {(title) " · Release Room"}link rel="stylesheet" href="/assets/app.css";script src="/assets/app.js" defer {}}
 body {header class="topbar" {nav aria-label="Main navigation" {a class="brand" href="/" {"Release Room"}a class="nav-link" href="/" {"Queue"}a class="connection-link" href="/connection" {"YouTube connection"}}}
 (content)
 footer {"Release Room" span {"/"} "Built for your next launch"}
 }} }.into_string()
}
pub fn queue(
    app: &App,
    jobs: &[Job],
    connected: bool,
    channel: Option<&str>,
    filter: &str,
) -> String {
    let visible: Vec<_> = jobs
        .iter()
        .filter(|j| match filter {
            "upcoming" => matches!(j.state, Status::Queued | Status::Uploading | Status::Retry),
            "attention" => j.state.attention(),
            "scheduled" => j.state == Status::Scheduled,
            _ => true,
        })
        .collect();
    page(
        app,
        "Video queue",
        html! {main class="queue-page" {div class="page-heading" {div {h1 {"Video queue"}p class="subtitle" {"Upload early. Publish when it counts."}}a class="button" href="/jobs/new" {"Schedule video"}}
        div class="notice" {span {@if connected {"Connected to " strong {(channel.unwrap_or("your YouTube channel"))} ". Uploads run automatically."} @else {"Connect your YouTube channel to start uploading."}} a href="/connection" {@if connected {"Manage connection"} @else {"Connect YouTube"}}}
        nav class="filters" aria-label="Filter videos" {@for (key,label) in [("all","All videos"),("upcoming","Upcoming"),("attention","Needs attention"),("scheduled","Scheduled")] {a class=(if filter==key {"selected"}else{""}) aria-current=(if filter==key {"page"}else{"false"}) href=(format!("/?filter={key}")) {(label)}}}
        @if visible.is_empty() {section class="empty-state" {(video_icon()) @if jobs.is_empty() {h2 {"Your next release starts here"}p {"Schedule a trailer, devlog, or announcement for your games."}a class="button" href="/jobs/new" {"Schedule your first video"}} @else {h2 {"No videos in this view"}p {"Try another filter to see the rest of your queue."}a class="button secondary" href="/" {"Show all videos"}}}}
        @else {div class="job-list" {div class="list-heading" {span {"Video"}span {"Status"}span {"Upload"}span {"Publication"}} @for j in visible {a class="job-row" href=(format!("/jobs/{}",j.id)) {div class="video-cell" {span class="mini-video" {(video_icon())}div {strong {(j.title)}small {(format!("{:.1} MB",j.size as f64/1_000_000.0))}}}div {span class=(format!("status {}",j.state.as_str())) {(j.state.as_str())} @if j.state==Status::Uploading {progress value=(j.bytes_sent) max=(j.size) aria-label="Upload progress" {}small {(format!("{}%",j.bytes_sent*100/j.size.max(1)))}}}div class="date-cell" {span class="mobile-label" {"Upload"} time datetime=(iso(j.upload_at)) {(iso(j.upload_at))}}div class="date-cell" {span class="mobile-label" {"Publication"}time datetime=(iso(j.publish_at)) {(iso(j.publish_at))}}}}}}
        p class="queue-note" {"Videos upload privately. YouTube handles publication at the scheduled time."}
        }},
    )
}
pub fn schedule(app: &App) -> String {
    page(
        app,
        "Schedule video",
        html! {main class="form-page" {div class="form-heading" {a href="/" {"← Back to queue"}h1 {"Schedule video"}}
        form id="schedule-form" action="/api/jobs" method="post" enctype="multipart/form-data" class="editor" {
        div id="form-error" role="alert" hidden {}div class="field" {label for="video" {"Video file"}input type="file" id="video" name="video" accept="video/*,.mkv" required;small {"MP4, MOV, WebM, or MKV. Stored on this server before uploading to YouTube."}video id="local-preview" controls preload="metadata" hidden {}}
        div class="field" {label for="title" {"Title"}input id="title" name="title" placeholder="Enter a title" maxlength="100" required;}
        div class="field" {label for="description" {"Description"}textarea id="description" name="description" rows="4" placeholder="Add a description (optional)" {}}
        div class="field" {label for="tags" {"Tags"}input id="tags" name="tags" placeholder="Add tags separated by commas";small {"Example: devlog, gameplay, indie game"}}
        div class="two-column" {div class="field" {label for="upload-at" {"Upload time " span class="muted" {"(optional)"}}input type="datetime-local" id="upload-at" name="upload_local";small {"Leave empty to start on the next worker pass."}}div class="field" {label for="publish-at" {"Publish time"}input type="datetime-local" id="publish-at" name="publish_local" required;small {"When the video should go public."}}}
        div class="field" {label for="timezone" {"Timezone"}select id="timezone" name="timezone" {option value="local" {"Browser local timezone"}option value="+00:00" {"UTC (+00:00)"}option value="+01:00" {"UTC +01:00"}option value="+02:00" {"UTC +02:00"}option value="-04:00" {"UTC −04:00"}option value="-05:00" {"UTC −05:00"}}small id="time-summary" {"All dates are saved as UTC. Leave time for uploading and processing."}}
        div class="two-column" {div class="field" {label for="audience" {"Audience"}select id="audience" name="made_for_kids" required {option value="" {"Choose the video's audience"}option value="false" {"Not made for kids"}option value="true" {"Made for kids"}}}div class="field" {label for="synthetic" {"Synthetic media"}label class="checkbox" {input id="synthetic" type="checkbox" name="synthetic_media";"Contains realistic altered or synthetic content"}}}
        div class="field" {label for="category" {"Category"}select id="category" name="category" {option value="20" {"Gaming"}option value="22" {"People & Blogs"}option value="24" {"Entertainment"}}}
        div class="form-footer" {span id="transfer-status" role="status" {}button type="submit" {"Add to queue"}}
        }} },
    )
}
pub fn detail(app: &App, j: &Job) -> String {
    page(
        app,
        &j.title,
        html! {main class="detail-page" {a class="back" href="/" {"← Back to queue"}div class="page-heading" {div {h1 {(j.title)}p class="subtitle" {"Video details and publication schedule"}}span class=(format!("status {}",j.state.as_str())) {(j.state.as_str())}}
        @if let Some(error)=&j.last_error {div class="notice error" role="alert" {(error)}}
        div class="detail-grid" {section {video class="preview" controls preload="metadata" src=(format!("/media/{}",j.id)) {}h2 {"Description"}p class="description" {@if j.description.is_empty(){"No description added."}@else{(j.description)}}h2 {"Tags"}p {(j.tags.join(", "))}}
        aside class="details" {h2 {"Release schedule"}dl {dt {"Upload starts"}dd {time datetime=(iso(j.upload_at)) {(iso(j.upload_at))}}dt {"Publication"}dd {time datetime=(iso(j.publish_at)) {(iso(j.publish_at))}}dt {"Audience"}dd {@if j.made_for_kids {"Made for kids"}@else{"Not made for kids"}}dt {"Synthetic media"}dd {@if j.synthetic_media {"Disclosed"}@else{"Not declared"}}dt {"Upload progress"}dd {progress value=(j.bytes_sent) max=(j.size) aria-label="Upload progress" {}(format!(" {:.1} / {:.1} MB",j.bytes_sent as f64/1e6,j.size as f64/1e6))}dt {"Attempts"}dd {(j.attempts)}}
        @if let Some(id)=&j.video_id {a class="button secondary" href=(format!("https://studio.youtube.com/video/{id}/edit")) target="_blank" rel="noopener noreferrer" {"Open YouTube Studio"}}
        @if j.state==Status::Scheduled {p class="muted" {"YouTube accepted this schedule. Confirm processing and eventual publication in Studio."}}
        @if j.state.attention() {form class="retry-form" data-job=(j.id) {label for="retry-publish" {"Retry with publication time"}input id="retry-publish" type="datetime-local" required;small {"Uses your browser's local timezone."}button type="submit" {"Retry upload / schedule"}}}
        @if j.session_url.is_none() && j.video_id.is_none() && matches!(j.state,Status::Queued|Status::Retry|Status::Failed|Status::Missed) {button class="danger secondary" data-cancel=(j.id) {"Cancel video"}}
        div id="action-status" role="status" {}
        }} }},
    )
}
pub fn connection(app: &App, connected: bool, name: Option<&str>, configured: bool) -> String {
    page(
        app,
        "YouTube connection",
        html! {main class="form-page" {div class="form-heading" {a href="/" {"← Back to queue"}h1 {"YouTube connection"}}
        section class="editor connection" {h2 {@if connected {"Your channel is connected"}@else{"Connect your studio's channel"}}p {@if connected{(name.unwrap_or("Existing YouTube credentials")) ". Queued videos will upload to this account."}@else{"Authorize Release Room to upload videos privately and schedule their publication."}}
        @if configured {form action="/auth/start" method="post" {input type="hidden" name="csrf" value=(app.csrf);button type="submit" {@if connected {"Reconnect YouTube"}@else{"Connect YouTube"}}}p class="muted" {"Choose the intended studio channel in Google. This queue stays tied to that channel."}}
        @else {h2 {"One-time setup"}ol {li {"Enable YouTube Data API v3 in your Google Cloud project."}li {"Create a Web application OAuth client."}li {"Add this authorized redirect URI:" code {(format!("{}/auth/callback",app.public_url))}}li {"Save the downloaded JSON as client_secret.json in your data directory, or set YOUTUBE_CLIENT_SECRETS to its path. Then refresh this page."}}}
        div class="notice" {p {"YouTube restricts uploads from unaudited API projects to private viewing. Your project must be eligible for public publishing. OAuth apps in Testing may need reconnection after seven days."}}
        }} },
    )
}
pub fn error(app: &App, message: &str) -> String {
    page(
        app,
        "Something needs attention",
        html! {main class="form-page" {h1 {"Something needs attention"}div class="notice error" role="alert" {(message)}a class="button secondary" href="/" {"Back to queue"}}},
    )
}
