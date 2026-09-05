# Release Room design

Reference: [generated concept](design-concept.png), created with the built-in Image Gen tool.
Brief: a complete in-house game studio YouTube queue and scheduling form, restrained orange
accent, white background, no invented metrics or game imagery, server-rendered Rust UI.

Tokens: white background, #f6f7f9 notice surface, #11151d ink, #657080 secondary text,
#e1e4e9 borders, #ff5900 accent; 8px corners; system sans-serif. Headings 42/28/22px,
body 16px, controls 14px. Main content max-width 1200px, 40px desktop gutters.
Navigation is a horizontal top bar. Filters are underlined tabs, not pills. Empty queue is
a spacious dashed-border region. Use small outline video/link icons, no raster UI.

Primary copy: Release Room; Queue; Connect YouTube; Video queue; Upload early. Publish when
it counts.; Schedule video; All videos; Upcoming; Needs attention; Scheduled; Your next
release starts here; Schedule a trailer, devlog, or announcement for your games.; Schedule
your first video; Videos upload privately. YouTube handles publication at the scheduled time.

Scheduling uses the reference form as a separate full-width route with a 760px content limit.
Use an explicit UTC offset rather than the concept's invented US timezone default. Default
dates use the browser's current local zone with DST-aware conversion; the offset remains editable.
Audience has no preselected answer. File upload size and accepted types reflect implementation.

Required workflow extensions in the same visual system: populated queue rows with title, state,
upload progress and dates; detail view with video playback, metadata, retry/cancel actions;
connection setup/status and actionable errors. No example jobs in the real database. Mobile
stacks heading/actions, wraps filters, and converts table rows into open stacked rows.
