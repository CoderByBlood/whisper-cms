# 08 Cross-Cutting Concepts
## 8.1 Overview
**Summary:** WhisperCMS applies explicit, system-wide concepts to ensure safety, performance, determinism, and extensibility.

Crosscutting concepts are consistent patterns implemented across all system layers — server, desktop, and extension environments.  
They reinforce WhisperCMS’s priorities: **Safety → Performance → UX → DX**, governed by the **Zero-Cost Abstractions** rule.

## 8.2 Safety and Isolation
**Summary:** Safety is enforced through process, capability, and trust boundaries.

- **Sandboxed Extensions:**  
  - All plugins and themes execute within `rhai` sandboxes.  
  - No direct filesystem, network, or environment access without declared capability.  
  - Capabilities validated at load time and logged for audit.

- **Ingress Safety:**  
  - `pingora` validates every external request (method, headers, CORS, rate limits).  
  - Rejects unsafe input before it reaches Axum routes.

- **Policy Enforcement:**  
  - `cedar` evaluates authorization policies declaratively before any privileged action.  
  - Fine-grained roles for admin, system, and extension identities.

- **Secret Management:**  
  - `rops` provides encrypted secret storage, isolated per environment.  
  - Secrets never exposed to extension contexts.

- **Deterministic Execution:**  
  - No dynamic linking or runtime reflection.  
  - All execution paths reproducible from versioned configuration and content.

## 8.3 Performance and Zero-Cost Abstractions
**Summary:** Every abstraction must add negligible runtime, memory, or security overhead.

- **Language Choice:** Rust guarantees predictable performance without garbage collection.  
- **Async and Streaming I/O:** `tokio`, `axum`, and `lol_html` enable non-blocking throughput.  
- **Pay-for-What-You-Use:** Optional features compiled conditionally; unused modules incur no cost.  
- **Local Reasoning:** Functions and abstractions are small, explicit, and testable in isolation.  
- **Benchmark Verification:** Profiling and load tests confirm near-zero abstraction tax before merges.  

## 8.4 Configuration and Environment
**Summary:** Configuration is explicit, versioned, and human-readable.

- **Primary Config Files:** `settings.toml`, `config.toml` per site instance.  
- **Version Control:** All configuration stored and versioned via Git.  
- **Hierarchical Overrides:** Local overrides validated and merged deterministically.  
- **Immutable Startup:** Configurations loaded once on startup; runtime changes require commit + restart.  
- **Cross-Environment Consistency:** Git branches separate staging, preview, and production states.

## 8.5 Persistence and Data Integrity
**Summary:** Git provides immutability, SQLite provides performance.

- **Dual Persistence Model:**  
  - **Git:** canonical system-of-record, audit trail, and deployment channel.  
  - **SQLite / LibSQL:** runtime transactional cache for reads and queries.

- **Synchronization:**  
  - Admin commits changes locally → pushed to remote Git → server pulls and rebuilds cache.  
  - No direct remote database writes.

- **Data Validation:**  
  - Schema-checked serialization using `gray_matter`, `serde_yaml`, and `toml_edit`.  
  - Invalid data rejected before persistence.

- **Durability Guarantees:**  
  - Git commits atomic and cryptographically signed.  
  - SQLite transactions ACID-compliant.

## 8.6 Eventing and Messaging
**Summary:** Event-driven communication decouples core, plugins, and themes.

- **Event Bus:** Implemented via `leptos_reactive` (reactive signal/effects).  
- **Event Types:** request, content, response, and system events.  
- **Reactive Model:**  
  - Core emits typed signals.  
  - Plugins register signal in effects.  
  - Event Bus executes matching plugin effects asynchronously.  
- **Safety:**  
  - Plugin timeouts and quotas enforced.  
  - Event order deterministic; cyclic dependencies are explicit and testable.  
- **Transparency:**  
  - All events logged for observability via `tracing`.

## 8.7 Extensibility and Plugins/Themes
**Summary:** Extensibility is strictly bounded and declarative.

- **Extension Types:**  
  - **Plugins:** augment behavior, workflows, or content transformations.  
  - **Themes:** define presentation and routing.

- **Manifest-Driven:** Each extension declares:  
  - Capabilities (filesystem, network, config access).  
  - Version compatibility and required APIs.  
  - Configuration schema.

- **Lifecycle:**  
  1. Installed via Git or marketplace.  
  2. Verified and loaded at startup.  
  3. Executed through event subscriptions.  
  4. Disabled or removed via Git commit.

- **Hot-Reload Isolation:** Restart required for activation; prevents live-mutation of trusted code.

## 8.8 Authentication, Authorization, and Identity
**Summary:** Layered identity model separates system, admin, and extension scopes.

| Identity Type | Description |
| ------------- | ----------- |
| **System Identity** | Internal processes (server, CLI) using local credentials. |
| **Admin Identity** | Desktop users authenticated via local OS or Git credentials. |
| **Extension Identity** | Capability-scoped, non-persistent identities within sandbox. |

- **Authorization Engine:** `cedar` evaluates policies for every sensitive operation.  
- **Session Handling:** `tower-sessions` provides HMAC-signed cookies for web sessions.  
- **Future Integration:** optional `webauthn-rs` planned for hardware-based admin login.

## 8.9 Observability, Logging, and Telemetry
**Summary:** Observability is built into the core runtime via `tracing`.

- **Tracing Layers:** Correlate spans across ingress, event, and persistence layers.  
- **Structured Logs:** Every operation emits JSON-formatted structured logs for analysis.  
- **Metrics:** Built-in counters for request latency, event processing time, and sandbox invocations.  
- **Debug Mode:** Extended instrumentation with source-linked spans for developers.  
- **Crash Safety:** Uncaught errors converted to structured `snafu` reports and stored in logs.  

## 8.10 Error Handling and Resilience
**Summary:** Faults are isolated and contained to preserve uptime.

- **Error Typing:** `snafu` provides typed, contextual errors.  
- **Circuit Breaking:** `failsafe` and `tower-resilience-circuitbreaker` protect external calls.  
- **Retry Policies:** deterministic retry with exponential backoff for transient Git or I/O errors.  
- **Containment:** Plugin failures logged and quarantined; core continues unaffected.  
- **Graceful Shutdown:** Interrupt signals cause controlled teardown and sync before exit.  

## 8.11 Versioning and Evolution
**Summary:** All change is explicit and traceable through Git.

- **Git Commits:** Represent atomic state transitions.  
- **Schema Stability:** Content models versioned through migrations, never implicit changes.  
- **Extension Compatibility:** Semantic versioning enforced at load time.  
- **Upgrade Path:** CLI tools validate and upgrade configuration schemas safely.  
- **Backward Compatibility:** Maintained at the API boundary; adapters bridge older plugin versions.  

## 8.12 Internationalization (L10N/I18N)
**Summary:** WhisperCMS is locale-neutral at its core.

- **Core Independence:** No built-in multilingual assumption.  
- **Optional Support:** Provided through themes or extensions.  
- **Storage:** Content stored in UTF-8, no locale-specific transformations.  
- **Future Consideration:** Internationalization handled as a higher-level plugin capability.

## 8.13 Testing and Verification
**Summary:** Determinism allows exhaustive testing and reproducibility.

- **Unit Tests:** Validate domain logic and event semantics.  
- **Integration Tests:** Simulate full startup + delivery flows in isolated environments.  
- **Property-Based Tests:** Verify determinism of content serialization and Git commits.  
- **Security Tests:** Sandboxing boundaries fuzz-tested for isolation regressions.  
- **Benchmark Suites:** Ensure zero-cost abstraction claims hold under load.

## 8.14 Crosscutting Concept Summary
| Concept | Core Mechanism | Primary Crates / Technologies |
| ------- | -------------- | ----------------------------- |
| **Safety** | Sandbox + Policy Engine | `rhai`, `cedar`, `rops` |
| **Performance** | Async + Zero-Cost Abstractions | `tokio`, `axum`, `lol_html` |
| **Persistence** | Git + SQLite | `libsql`, `sqlx`, `gray_matter` |
| **Eventing** | Reactive-based Event Bus | `leptos_reactive` |
| **Extensibility** | Capability-based Contracts | `rhai`, Git manifests |
| **Observability** | Distributed Tracing | `tracing` |
| **Error Handling** | Typed Contextual Errors | `snafu` |
| **Resilience** | Circuit Breaking + Retry | `failsafe`, `tower-resilience-circuitbreaker` |
| **Configuration** | Declarative, Versioned | `toml_edit`, Git |
| **Security** | Secrets + Authorization | `rops`, `cedar` |

## 8.15 Summary
**Summary:** Crosscutting concepts unify WhisperCMS’s architectural priorities through reproducible, type-safe mechanisms.

- Safety enforced at every layer.  
- Performance ensured through zero-cost abstractions.  
- Extensibility achieved via sandboxed, event-driven design.  
- Determinism guaranteed by Git-based version control.  
- Observability and resilience embedded in the runtime.

**Result:**  
WhisperCMS behaves predictably, scales safely, and remains maintainable over time — a content engine engineered for integrity and longevity.