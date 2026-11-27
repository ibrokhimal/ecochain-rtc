# Workplace meeting backend

The server advertises `work_tools_v1` in admitted join responses. Updated clients
can then display shared drawing, whiteboards, notes/tasks/polls/questions and host
moderation. Older clients remain compatible with ordinary media. After deployment,
clients must rejoin to negotiate the new capability. Annotation requires both an
updated client and this updated backend; an old client gains no toolbar from a
server update alone.

Basic collaboration, lobby, locks and host mute work with the existing configuration.
No speech/Jira credentials are needed for drawing. See [the protocol](PROTOCOL.md)
for actions, revision patches, join snapshots and instance-scoped archive APIs.

## Optional services

Use the commented examples in `server/config/production.example.toml`:

- `[storage]`: private persistent volume, base64 32-byte wrapping key from
  `master_key_env`, retention (default 30 days). Keys belong in secret management.
  Archives are encrypted; losing the key loses access. Use one server writer per
  volume. In Docker, add a writable persistent mount: the shipped compose service
  otherwise has a read-only filesystem and no archive mount.
- `[recording]`: FFmpeg executable, private capacity-limited processing directory,
  duration/input-byte bounds. The deployed runtime image must include FFmpeg (the
  existing minimal Docker runtime does not). Mount a writable private processing
  volume as well. Short-lived plaintext spools are removed after work; startup only
  removes UUID worker directories with the exact private marker.
- `[providers.stt]`: HTTPS OpenAI-compatible multipart transcription endpoint,
  model, API-key environment variable, JSON/verbose JSON format, optional language.
  Requires storage and the recording/FFmpeg worker configuration.
- `[providers.summary]`: HTTPS JSON-response-capable chat-completion endpoint,
  explicit model and API-key environment variable. Generated content stays draft
  until a host reviews and applies it with a matching workspace revision.
- `[providers.jira]`: site origin, service email/API-token environment variable,
  project allowlist, actual issue-type ID and identity/account mapping. Host approval
  binds the exact payload and destination. Uncertain creates only reconcile; retries
  never blindly repeat POST. Scoped api.atlassian.com/ex/jira tokens are unsupported.

Provider credentials never enter clients. Storage/recording/providers remain disabled
when absent from configuration. Basic drawing does not depend on these sections.
Interrupted recording/transcript/summary jobs become failed at restart; pending Jira
work becomes uncertain. Job metadata has count, per-file and aggregate bounds.

## Verification and delivery

The backend extraction from `feat/work-meeting-tools` includes only `server/` and
backend protocol/setup documentation. Flutter changes remain on the feature branch.
The full source feature received a partial independent review followed by author
self-review; a complete independent approval is not claimed.

Verify with `cargo test --locked`, `cargo clippy --all-targets -- -D warnings` and
`cargo fmt --check` in `server/`. FFmpeg-backed tests are opt-in:

```sh
cargo test --test recording -- --ignored
cargo test --test captions -- --include-ignored
```

Tests use local STT/summary/Jira mocks, encrypted temporary archives and actual local
FFmpeg. Live external-provider accounts, production deployment and real multi-device
A/V acceptance are separate from merging the source. The browser player synchronizes
recording segments, audio, board events and optional transcript subtitles.

Merge verification: `cargo test --locked --offline -- --test-threads=1` passed
186 tests, with three opt-in media tests ignored. Formatting and Clippy with
warnings denied passed. The first parallel run hit the existing video-switch
keyframe-count limit (4 rather than ≤3); that test passed in isolation and in the
full serial suite. No thresholds were relaxed or unrelated media code changed.
