# WhisperCMS – Design Decisions (Locked)

**Status:** Locked  
**Version:** 2025-08-08 19:05:09Z  
**Scope:** All decisions made to date. Organized by **priority**. Each decision
includes _Rationale_, _Alternatives Considered_, _Tradeoffs_, _Implications_,
and any _Open Questions_.

---

## Motivation

WhisperCMS aims to provide users a safe, fast, and flexible general purpose
content management system using newer proven technologies and uses resources
effectively for small and large deployments.

---

## Design Priorities (Ranked)

1. **Safety:** The CMS should leverage as much static compile-time checking as
   possible
2. **Performance:** The CMS should be lightning fast with deterministic
   performance without compromising safety
3. **User Experience:** The CMS should be highly usable from author, editor, and
   administrator perspectives without compromising performance or safety
4. **Developer Experience:** The CMS should be easy to extend without
   compromising the user experience, performance, or safety

---

## Table of Contents

- [WhisperCMS – Design Decisions (Locked)](#whispercms--design-decisions-locked)
  - [Motivation](#motivation)
  - [Design Priorities (Ranked)](#design-priorities-ranked)
  - [Table of Contents](#table-of-contents)
  - [🛡️ Safety](#️-safety)
    - [Secure Session Management](#secure-session-management)
    - [Actor Supervision](#actor-supervision)
    - [Plugin Execution Timeouts (Env-Configurable)](#plugin-execution-timeouts-env-configurable)
    - [Priority-Ordered Hooks \& Safe Short-Circuiting](#priority-ordered-hooks--safe-short-circuiting)
    - [Admin Plugins Lazy Loading](#admin-plugins-lazy-loading)
    - [Plugin-to-Plugin Messaging via Event Bus](#plugin-to-plugin-messaging-via-event-bus)
    - [Hybrid Capability Model](#hybrid-capability-model)
    - [Content Visibility (Public / Private / Password Groups)](#content-visibility-public--private--password-groups)
    - [Post Locking / Concurrent Editing](#post-locking--concurrent-editing)
    - [Autosave \& Recovery](#autosave--recovery)
    - [Content \& Metadata Storage in Git + Rebuilt SQLite](#content--metadata-storage-in-git--rebuilt-sqlite)
    - [No Public Diagnostics Endpoint](#no-public-diagnostics-endpoint)
    - [Maintenance Mode via `maintenance.lock`](#maintenance-mode-via-maintenancelock)
  - [⚡ Performance](#-performance)
    - [Rendering Responsibility in Themes (SSR + Headless)](#rendering-responsibility-in-themes-ssr--headless)
    - [Theme Mounts (incl. JSON headless at `/api/`)](#theme-mounts-incl-json-headless-at-api)
    - [Pretty Permalinks](#pretty-permalinks)
    - [Canonical Redirects (301 Old Paths)](#canonical-redirects-301-old-paths)
    - [Global Search (SQLite FTS5)](#global-search-sqlite-fts5)
    - [Scheduled Publishing](#scheduled-publishing)
    - [Sticky Posts](#sticky-posts)
    - [Featured Images / Thumbnails](#featured-images--thumbnails)
    - [Excerpts (Auto + Filterable)](#excerpts-auto--filterable)
    - [Caching Strategy: CDN/Proxy over In-Process Cache](#caching-strategy-cdnproxy-over-in-process-cache)
  - [🎨 User Experience](#-user-experience)
    - [Markdown + Shortcodes + Filters](#markdown--shortcodes--filters)
    - [Menus via Theme Hooks](#menus-via-theme-hooks)
    - [Revision History + Trash/Undelete](#revision-history--trashundelete)
    - [Author Archives](#author-archives)
    - [Roles \& Custom Roles UI](#roles--custom-roles-ui)
  - [🧑‍💻 Developer Experience](#-developer-experience)
    - [Programming Language: Rust](#programming-language-rust)
    - [Extensibility Model: Static Compilation](#extensibility-model-static-compilation)
    - [Typed Plugin/Theme API (HookMessage Enums)](#typed-plugintheme-api-hookmessage-enums)
    - [RequestContextBuilder](#requestcontextbuilder)
    - [Theme Manifest “Headless” Flag](#theme-manifest-headless-flag)
    - [Test Harness \& Mocks](#test-harness--mocks)
    - [CLI Tool (`whisper`)](#cli-tool-whisper)
    - [Custom Content Types](#custom-content-types)
    - [Config in Git (`/config/`)](#config-in-git-config)
    - [Taxonomy Storage in Git](#taxonomy-storage-in-git)
    - [Out-of-Core Features via Plugins](#out-of-core-features-via-plugins)
  - [Shared Implications](#shared-implications)
  - [Open Questions](#open-questions)

---

## 🛡️ Safety

### Secure Session Management

**Decision.** HMAC-signed session tokens with embedded timestamps; optional
user-agent/IP binding; short expirations; middleware protection for
install/config routes.  
**Rationale.** Minimize replay/token theft risk while keeping runtime stateless
and simple.  
**Alternatives Considered.**

- JWT (asymmetric): larger tokens, unnecessary complexity for same trust model.
- Server-stored sessions: introduces state and coordination overhead.  
  **Tradeoffs.** Rotation and clock-skew policy needed; otherwise simpler
  infra.  
  **Implications.** Clear cookie scope/expiry; rotate keys safely.  
  **Open Questions.** None.

### Actor Supervision

**Decision.** All plugin/admin/theme workers run under a `PluginSupervisor` with
restart/backoff; safe state recovery.  
**Rationale.** Fault isolation and resilience.  
**Alternatives Considered.** OS-only supervision (too coarse), unsupervised
tasks (unsafe).  
**Tradeoffs.** Small orchestration complexity; large stability win.  
**Implications.** Standard failure taxonomy + policy tuning.  
**Open Questions.** None.

### Plugin Execution Timeouts (Env-Configurable)

**Decision.** Per-message timeouts with environment profiles; applies to
plugins, admin, themes.  
**Rationale.** Bound tail latency and prevent hangs.  
**Alternatives Considered.** Unlimited runtimes (unsafe), global single timeout
(too blunt).  
**Tradeoffs.** Cancellation plumbing; predictable performance.  
**Implications.** Logs with hook/timeout reason; user-safe error pages.  
**Open Questions.** None.

### Priority-Ordered Hooks & Safe Short-Circuiting

**Decision.** Hooks carry `priority: u8`; stable within same value.
Filters/actions may short-circuit; themes/admin can detect via
`ctx.was_short_circuited()` and reason.  
**Rationale.** Deterministic behavior and controlled early exits.  
**Alternatives Considered.** Non-deterministic order (bad DX), no
short-circuiting (wasted work).  
**Tradeoffs.** Requires clear contract docs.  
**Implications.** Dev-only introspection page shows
order/source/short-circuit.  
**Open Questions.** None.

### Admin Plugins Lazy Loading

**Decision.** Admin plugin actors start only after admin login. No opt-in for
early boot.  
**Rationale.** Keep public boot and steady-state lean.  
**Alternatives Considered.** Always-on admin plugins (waste), full dynamic lazy
for all plugins (perf risk).  
**Tradeoffs.** First admin request pays warmup.  
**Implications.** Log warmup; cache where appropriate.  
**Open Questions.** None.

### Plugin-to-Plugin Messaging via Event Bus

**Decision.** No shared state; communication strictly via internal event bus.  
**Rationale.** Avoids races and hidden coupling.  
**Alternatives Considered.** Shared mutable state; global registries.  
**Tradeoffs.** Slight verbosity; big safety gain.  
**Implications.** Versioned message schemas as needed.  
**Open Questions.** None.

### Hybrid Capability Model

**Decision.** `Capability = KnownCapability | DynamicCapability(String)`;
enforced across edit/publish/visibility.  
**Rationale.** Type safety for core + extensibility for plugins/admins.  
**Alternatives Considered.** Pure strings (error-prone), fully static enums (not
extensible).  
**Tradeoffs.** Some runtime checks; strong DX.  
**Implications.** Roles bundle capabilities; admin UI for management.  
**Open Questions.** None.

### Content Visibility (Public / Private / Password Groups)

**Decision.** Private gated by capability; password groups hashed with Argon2id;
success sets scoped HMAC cookie.  
**Rationale.** Common use cases without complex ACLs.  
**Alternatives Considered.** Per-post ad-hoc ACLs (heavy), single-password per
post (unscalable).  
**Tradeoffs.** Simple group management UI needed.  
**Implications.** Clear cookie scope/expiry semantics.  
**Open Questions.** None.

### Post Locking / Concurrent Editing

**Decision.** TTL-based lock (user_id, timestamp); admin UI supports
takeover/recovery; supervisor prunes expired locks.  
**Rationale.** Prevents “last save wins” issues.  
**Alternatives Considered.** Pure optimistic concurrency; CRDT merge
(overkill).  
**Tradeoffs.** Occasional takeover flow; safer editing.  
**Implications.** Clear UX cues and audit logs.  
**Open Questions.** None.

### Autosave & Recovery

**Decision.** Editor **autosave writes directly to the tracked content file** in the Git working tree; **no separate autosave store** and **no auto-commit**. The working tree holds the autosaved state; the editor decides when to commit (Save Draft / Publish).  
**Rationale.** Lean on Git for durability, diffs, and recovery; avoid parallel stores and drift.  
**Alternatives Considered.**

- Separate autosave DB/store: introduces drift and complexity; duplicates Git’s job.
- Auto-commit every autosave: creates noisy history, merge pain, and confusing notifications.  
**Tradeoffs.** Uncommitted local changes exist; needs clear UX around “unsaved work” and conflicts. With file-level locking, race risk is minimized; the UI will warn and diff if another user saves first.  
**Implications.** Editor periodically writes to the file on disk; on open, load working-tree contents; before publish, validate and commit with a message. Optionally create a safety “stash” prior to destructive operations. File change watchers trigger previews/re-index.  
**Open Questions.** None.

### Content & Metadata Storage in Git + Rebuilt SQLite

**Decision.** Markdown with **TOML frontmatter fenced by `+++`**; **no volatile
fields** in frontmatter. On boot, parse into a **persistent SQLite** DB that is
**deleted and rebuilt** each boot (Git = source of truth). Plugins **cannot**
write frontmatter directly; plugin/theme settings live under **`/config/`** TOML
files.  
**Rationale.** Git-native workflows + fast queries without RAM bloat.  
**Alternatives Considered.**

- Parallel metadata files: fragments workflows; raises drift risk.
- In-memory DB only: wasteful at scale; poor introspection.
- Postgres primary: heavier ops; SQLite + OS page cache is sufficient.  
  **Tradeoffs.** Boot rebuild cost (mitigated by caching); vastly simpler
  integrity model.  
  **Implications.** Strict TOML validation; clear error reporting (file/line).  
  **Open Questions.** None.

### No Public Diagnostics Endpoint

**Decision.** No public diagnostics. Admin-only UI and infra logs/metrics cover
needs.  
**Rationale.** Reduce attack surface.  
**Alternatives Considered.** Healthz with internals; debug endpoints (leaky).  
**Tradeoffs.** Ensure admin tools are solid.  
**Implications.** Observability hooks elsewhere.  
**Open Questions.** None.

### Maintenance Mode via `maintenance.lock`

**Decision.** Core honors a `maintenance.lock` file to serve a maintenance
response while keeping admin access.  
**Rationale.** Simple, safe ops toggle.  
**Alternatives Considered.** DB flag; proxy-only rules.  
**Tradeoffs.** Tiny hot-path check.  
**Implications.** Document location + semantics.  
**Open Questions.** None.

---

## ⚡ Performance

### Rendering Responsibility in Themes (SSR + Headless)

**Decision.** Themes own rendering; CMS bypasses theming only for static assets;
filters are invoked explicitly by templates.  
**Rationale.** Clear separation of concerns; flexibility.  
**Alternatives Considered.** Core-rendered pages; implicit filter injection.  
**Tradeoffs.** Themes must be explicit; better clarity.  
**Implications.** Theme SDK and examples.  
**Open Questions.** None.

### Theme Mounts (incl. JSON headless at `/api/`)

**Decision.** Map paths → themes via `config/theme_mounts.toml` in Git. Fail
fast on unknown theme IDs. Provide an **official JSON theme** for `/api/`.  
**Rationale.** SSR/API/feeds coexist cleanly.  
**Alternatives Considered.** Single theme for all paths; router-level
switching.  
**Tradeoffs.** Slight config overhead; strong clarity.  
**Implications.** Simple mount editor in admin.  
**Open Questions.** None.

### Pretty Permalinks

**Decision.** Human/SEO-friendly paths with canonical routing.  
**Rationale.** Reader and SEO ergonomics.  
**Alternatives Considered.** Numeric IDs in URLs.  
**Tradeoffs.** None.  
**Implications.** See Canonical Redirects.  
**Open Questions.** None.

### Canonical Redirects (301 Old Paths)

**Decision.** Posts/terms support `old_paths[]`; router 301s to canonical URL.  
**Rationale.** Preserve link equity.  
**Alternatives Considered.** 404 old links.  
**Tradeoffs.** Small metadata cost.  
**Implications.** Admin UI for historical paths.  
**Open Questions.** None.

### Global Search (SQLite FTS5)

**Decision.** Core search via FTS5 over title/slug/excerpt/rendered
body/taxonomies; field qualifiers; ranked results with canonical URL +
snippet.  
**Rationale.** Fundamental CMS capability.  
**Alternatives Considered.** External engines; Tantivy/ES (heavier).  
**Tradeoffs.** Tuning tokenizer and ranking.  
**Implications.** Rebuild index on boot.  
**Open Questions.** None.

### Scheduled Publishing

**Decision.** Actor publishes at `publish_at`; logs + retries.  
**Rationale.** Editorial need.  
**Alternatives Considered.** Manual only.  
**Tradeoffs.** Time source + drift considerations.  
**Implications.** Admin scheduling UI.  
**Open Questions.** None.

### Sticky Posts

**Decision.** `is_sticky` metadata flag; templates may honor/ignore.  
**Rationale.** Simple prioritization.  
**Alternatives Considered.** Date-only ordering.  
**Tradeoffs.** None.  
**Implications.** Helper sort util.  
**Open Questions.** None.

### Featured Images / Thumbnails

**Decision.** Frontmatter field (e.g., `featured_image`); themes decide usage
and derivatives (media manager plugin assists).  
**Rationale.** Common, but presentational.  
**Alternatives Considered.** Core layout rules (too rigid).  
**Tradeoffs.** Responsibility on themes.  
**Implications.** Theme helpers.  
**Open Questions.** None.

### Excerpts (Auto + Filterable)

**Decision.** Auto-generated excerpt with override via filter; surfaced to
themes.  
**Rationale.** Lists/feeds/SEO snippets.  
**Alternatives Considered.** Manual only.  
**Tradeoffs.** Heuristic tuning.  
**Implications.** Provide sane defaults.  
**Open Questions.** None.

### Caching Strategy: CDN/Proxy over In-Process Cache

**Decision.** No internal server-side caching layer; emit cache-control headers;
rely on CDN/nginx/varnish.  
**Rationale.** Simpler core, fewer invalidation bugs.  
**Alternatives Considered.** In-process caches; DB caches.  
**Tradeoffs.** Ops dependency on proxy/CDN.  
**Implications.** Provide sample configs post-MVP.  
**Open Questions.** None.

---

## 🎨 User Experience

### Markdown + Shortcodes + Filters

**Decision.** Core editing model is Markdown; shortcodes & filters extend;
richer editors via plugins.  
**Rationale.** Power, safety, extensibility.  
**Alternatives Considered.** Built-in block builder; raw HTML by default.  
**Tradeoffs.** Minor learning curve.  
**Implications.** Live preview in admin.  
**Open Questions.** None.

### Menus via Theme Hooks

**Decision.** Themes expose hooks for menu locations and structure; admin config
writes to Git; themes render.  
**Rationale.** Menus are theme-dependent.  
**Alternatives Considered.** One-size-fits-all menu model.  
**Tradeoffs.** Per-theme variance accepted.  
**Implications.** Contract docs + examples.  
**Open Questions.** None.

### Revision History + Trash/Undelete

**Decision.** Revision history and undo are **powered by Git**. Saves/Publishes create commits with required messages; restore/revert operations use Git. **Trash/Undelete** is modeled as Git operations: delete is a commit that removes the file (or moves it to a `_trash/` path if the site prefers). Undelete restores the last committed version via `git restore` (or moves it back).  
**Rationale.** Git already provides effectively “infinite” revisions, diffs, and recovery with robust tooling.  
**Alternatives Considered.**

- DB-backed revision store: duplicates Git; adds schema and purge logic.
- Delta store separate from Git: complicates restore and auditing.
- Hybrid: highest complexity for little gain.  
**Tradeoffs.** Repository size grows with history; mitigate via Git GC/prune policies, LFS for large binaries, and keeping media out of frontmatter. Editorial semantics (draft vs published) map to frontmatter status and commit intent rather than branches.  
**Implications.** Admin must expose: view diff, revert to commit, and restore deleted content. “Purge” is a Git history operation (rare and destructive) — generally discouraged in production.  
**Open Questions.** None.

### Author Archives

**Decision.** Core author listing routes; author metadata extensible via
plugins.  
**Rationale.** Fundamental navigation.  
**Alternatives Considered.** Plugin-only.  
**Tradeoffs.** Minimal core routing.  
**Implications.** Theme presentation.  
**Open Questions.** None.

### Roles & Custom Roles UI

**Decision.** Admin can create users/roles and assign hybrid capabilities.  
**Rationale.** Control without code.  
**Alternatives Considered.** Hardcoded roles.  
**Tradeoffs.** Needs sensible defaults.  
**Implications.** Seed roles (Admin/Editor/Author).  
**Open Questions.** None.

---

## 🧑‍💻 Developer Experience

### Programming Language: Rust

**Decision.** Implement core/server in Rust.  
**Rationale.** Safety + performance.  
**Alternatives Considered.** Go/Node (weaker compile-time guarantees).  
**Tradeoffs.** Learning curve; runtime reliability gains.  
**Implications.** Standard toolchain.  
**Open Questions.** None.

### Extensibility Model: Static Compilation

**Decision.** Plugins/themes are statically compiled (workspace members or
vendored). No runtime install; for dev, use `cargo watch`.  
**Rationale.** Safety, determinism, supply-chain control.  
**Alternatives Considered.** Dynamic/hot reloading; marketplace auto-install.  
**Tradeoffs.** Rebuilds for upgrades; stronger guarantees.  
**Implications.** Versioning via Cargo; CI for releases.  
**Open Questions.** None.

### Typed Plugin/Theme API (HookMessage Enums)

**Decision.** `#[plugin]` + `inventory` for registration; `HookMessage` enums
for actions/filters/admin; explicit Request/Response types.  
**Rationale.** Avoid stringly-typed bugs.  
**Alternatives Considered.** String event names; JSON blobs.  
**Tradeoffs.** Some boilerplate; safer code.  
**Implications.** Macros/codegen for ergonomics.  
**Open Questions.** None.

### RequestContextBuilder

**Decision.** Deterministic classification into
`is_home/front/single/archive/search/404`; consumed by themes.  
**Rationale.** Predictable rendering.  
**Alternatives Considered.** Implicit/templated classification.  
**Tradeoffs.** A bit more plumbing; clarity.  
**Implications.** Shared helpers.  
**Open Questions.** None.

### Theme Manifest “Headless” Flag

**Decision.** Themes declare headless capability; used for mounts (e.g.,
`/api/`).  
**Rationale.** Avoid ambiguity; better tooling.  
**Alternatives Considered.** Inference.  
**Tradeoffs.** One more field; better intent.  
**Implications.** Validation on boot.  
**Open Questions.** None.

### Test Harness & Mocks

**Decision.** Provide plugin test-harness macro; recommend `mockall` for
seams.  
**Rationale.** Improve reliability without bloating core.  
**Alternatives Considered.** In-core dev sandbox.  
**Tradeoffs.** Tests live with plugins.  
**Implications.** Examples + fixtures.  
**Open Questions.** None.

### CLI Tool (`whisper`)

**Decision.** Separate CLI for ops/dev tasks; complements admin UI.  
**Rationale.** Ergonomics + clear boundaries.  
**Alternatives Considered.** In-app CLI (PHP-style).  
**Tradeoffs.** Two artifacts; better safety.  
**Implications.** Auth to repo/FS as needed.  
**Open Questions.** None.

### Custom Content Types

**Decision.** `register_content_type()` with typed schemas and hooks.  
**Rationale.** CMS fundamentals.  
**Alternatives Considered.** Only “post/page”.  
**Tradeoffs.** Maintain schema registry.  
**Implications.** Theme support for new types.  
**Open Questions.** None.

### Config in Git (`/config/`)

**Decision.** All settings under `/config/` (e.g., `config/theme_mounts.toml`,
`config/plugins/<slug>.toml`).  
**Rationale.** Versioned, reviewable, aligned with Git-first model.  
**Alternatives Considered.** DB options; per-plugin manifest files.  
**Tradeoffs.** Editors need basic TOML literacy; admin UI can assist.  
**Implications.** Clear layout conventions.  
**Open Questions.** None.

### Taxonomy Storage in Git

**Decision.** Hierarchical taxonomies as **paths** (e.g.,
`categories_path = ["root","sub"]`); flat taxonomies as **sets**
(`tags_set = ["a","b"]`). No numeric IDs or `parent_id` in frontmatter
(parentage implicit by path).  
**Rationale.** Git-diffable and merge-friendly.  
**Alternatives Considered.** Numeric IDs; separate taxonomy DB.  
**Tradeoffs.** Moves/renames require atomic updates (admin performs single
commit).  
**Implications.** Global taxonomy registry in Git.  
**Open Questions.** None.

### Out-of-Core Features via Plugins

**Decision.** Keep core lean; provide/enable via **plugins**:

- **Official plugins:** comments, feeds, breadcrumbs, embeds, media manager,
  editorial notifications, user registration & profiles.
- **Third-party plugins:** editorial workflow, SEO, CAPTCHA/Anti-spam,
  shortlinks.  
  **Rationale.** Smaller surface area; safer core.  
  **Alternatives Considered.** Baking into core.  
  **Tradeoffs.** More curation; much more flexibility.  
  **Implications.** Stable hooks; sample implementations.  
  **Open Questions.** None.

---

## Shared Implications

- **Docs & SDKs:** Theme/Plugin authoring guides; hook contracts; samples.
- **Ops:** Reverse-proxy snippets; maintenance-mode procedure; backup/restore
  (Git + SQLite rebuild).
- **Observability:** Tracing spans (request→context→hooks→render), histograms
  (TTFB), SQLite timings, boot rebuild time.
- **Compliance:** Clear cookie/password/retention policies.

---

## Open Questions

- None — this document is **locked**. Post-MVP enhancements (e.g., hook
  introspection UI, autosave recovery UI, Docker image, CLI install mode, etc.)
  are tracked separately.
