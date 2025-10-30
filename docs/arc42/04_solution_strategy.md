# 04 Solution Strategy
## 4.1 Architectural Pattern and Core Philosophy
**Summary:** WhisperCMS applies a strict hexagonal (ports and adapters) architecture to isolate domain logic from delivery and infrastructure.

WhisperCMS is architected as a **content engine**, not a framework.
Its hexagonal structure separates:
- **Domain Entities** and **Application Use Cases** (the core logic),
- **Interfaces and Adapters** (HTTP, CLI, desktop UI),
- **Infrastructure and Frameworks** (Git, SQLite, OS, and network).

This separation enables:
- Containment of untrusted extension code (plugins and themes),
- Parallel evolution of delivery platforms (server, desktop, CLI),
- Infrastructure independence — replacing databases or web frameworks without core changes.

The guiding philosophy: *extensibility without risk, and power without compromise.*

## 4.2 Priority-Driven Design Strategy
**Summary:** System behavior and trade-offs follow non-negotiable priorities: Safety → Performance → UX → DX, enforced by zero-cost abstractions.

1. **Safety:** No feature may widen trust boundaries or compromise data integrity.
   - Extensions run in sandboxed contexts (`rhai`).
   - Git commits form immutable audit trails.
   - No global mutable state or dynamic code injection.

2. **Performance:** Responsiveness is achieved through zero-cost abstractions.
   - Rust’s ownership model and predictable memory layout guarantee low overhead.
   - SQLite/LibSQL provides local transactional performance; Git remains the canonical record.
   - Async I/O and streaming transformations prevent latency amplification.

3. **User Experience (UX):** Predictable, local-first workflows.
   - The desktop admin console (Tauri + Svelte) mirrors WordPress familiarity without hidden coupling.
   - Workflows: configure → edit → preview → publish — no implicit network dependencies.

4. **Developer Experience (DX):** Explicit, typed APIs with safe extension points.
   - `rhai` scripting with constrained capabilities replaces untyped hooks.
   - Events and commands (via `leptos_reactive` signal/effect engine) replace global hooks.

> **Cross-cutting rule:** Zero-Cost Abstractions — no generalization or layer is accepted if it adds runtime, memory, or security cost beyond a direct implementation.

## 4.3 Technology Strategy
**Summary:** Rust and its ecosystem are chosen for type safety, performance, and isolation; all technologies adhere to the zero-cost constraint.

### Language Selection
WhisperCMS is implemented in **Rust** because it uniquely aligns with the system’s safety-first mandate and zero-cost abstraction constraint. Rust’s ownership and borrowing model enforces memory safety and eliminates entire classes of vulnerabilities such as buffer overflows and use-after-free, allowing WhisperCMS to fail closed without relying on garbage collectors or runtime checks. Its strong type system and trait-based generics enable expressive APIs that can be audited at compile time, ensuring extension authors cannot bypass sandbox boundaries. At the same time, Rust’s performance is on par with C and C++, giving predictable low-latency execution paths while still maintaining concurrency via async and actor models without data races. Rust’s ecosystem — from `tokio` for async runtimes to `snafu` for structured error handling and `ractor` for supervision trees — provides the building blocks needed for safe, actorized concurrency without runtime overhead. Choosing Rust also improves developer experience by providing deterministic builds, strong compiler guarantees, and a rapidly growing ecosystem of crates, while aligning with long-term maintainability and cross-platform portability. In short, Rust allows WhisperCMS to honor Safety, Performance, DX, and UX simultaneously under the uncompromising constraint of Zero-Cost Abstractions.

Languages traditionally used in CMS ecosystems were explicitly evaluated and rejected because they could not meet WhisperCMS’s priorities under the Zero-Cost Abstraction constraint. **PHP**, while historically dominant in CMSs like WordPress, was excluded due to its weak type system, runtime error handling, and reliance on mutable global state, all of which violate the safety-first mandate and prevent strict sandboxing. **Python** was dismissed because its interpreter and garbage collector impose unavoidable runtime overhead and nondeterministic pauses, undermining predictable budgets and low-latency guarantees, even though it excels at developer ergonomics. **Java** was rejected despite its mature ecosystem because the JVM adds unavoidable runtime cost, garbage collection latencies, and deployment complexity; its object-oriented model encourages inheritance hierarchies that run counter to WhisperCMS’s preference for lightweight functional and actor-oriented designs. **Go** was considered more seriously, but ultimately fell short: while it delivers simplicity and strong concurrency primitives, its garbage collector and lack of generics until recently made it impossible to guarantee zero-cost abstractions and fine-grained resource budget enforcement. In contrast, **Rust uniquely offers memory safety without garbage collection, predictable performance without hidden runtime costs, and compile-time enforcement of invariants that align perfectly with WhisperCMS’s safety and sandboxing requirements**.

| Concern | Technology / Crate | Purpose |
| ------- | ------------------ | ------- |
| **Web & Networking** | `pingora`, `axum` | Ingress controller and HTTP server |
| **Event System** | `leptos_reactive` | Reactive-based (signal/effect) event bus |
| **Extension Runtime** | `rhai` | Sandboxed scripting engine |
| **Data Access** | `libsql`, `sqlx` | Database engine and async access |
| **Templating & Parsing** | `minijinja`, `comrak`, `lol_html`, `jaq` | Rendering, markdown, HTML, and JSON transforms |
| **Serialization** | `gray_matter`, `toml_edit`, `serde_yaml` | Metadata and configuration |
| **Resilience & Errors** | `failsafe`, `tower-resilience-circuitbreaker`, `snafu` | Fault tolerance and error handling |
| **Security & Policy** | `cedar`, `rops` | Authorization and secrets management |
| **Desktop Admin** | `tauri`, custom `wcms:` scheme, `Leptos` | Local UI and backend bridge |

Each crate is selected for **safety, determinism, and cost predictability**, aligning with the architectural constraints.

## 4.4 Runtime Architecture and Interaction Strategy
**Summary:** Both server and desktop runtimes follow deterministic startup flows and strictly mediated communication paths.

### **Server Runtime**
1. Clone or scan Git repository → populate SQLite cache.
2. Load configuration (`settings.toml`).
3. Register plugins and themes.
4. Mount web routes to theme handlers.
5. Start ingress controller (`pingora`) and web server (`axum`).
6. Serve requests through the delivery pipeline:
   - Ingress validation → route match → plugin event dispatch → template rendering → response stream.

**Key property:** Plugins and themes interact only via the event bus — no direct access to core or filesystem.

### **Desktop Runtime**
1. Initialize or connect to a local Git repo.
2. Load or prompt for configuration (`settings.toml`).
3. Present administrative console (Tauri + Svelte WebView).
4. Execute `wcms:` protocol calls to the local Rust backend for all privileged operations.

The desktop app is **local-first** and offline-capable; Git provides synchronization and audit history.

## 4.5 Extensibility Strategy
**Summary:** Plugins and themes extend functionality within hard safety boundaries using event-driven contracts.

WhisperCMS supports two extension types:
- **Plugins** – modify or extend behavior (logic, workflow, content processing).
- **Themes** – control presentation and routing of rendered content.

Boundaries are strictly enforced:
- Plugins cannot modify presentation.
- Themes cannot perform logic outside rendering.
- Extensions communicate only via structured events and declared capabilities.

Each extension is version-controlled via Git and can be sourced locally or from a marketplace.
Configuration and state are atomic and reversible.

## 4.6 Data and Persistence Strategy
**Summary:** Git is the version controlled system-of-record; SQLite/LibSQL provides fast local transactional persistence.

- **Git:** Canonical source for configuration, content, and extension state.
  - Branches represent environments (e.g., `main`, `admin`, `preview`).
  - Commits provide traceability, rollback, and verification.
- **SQLite/LibSQL:** Runtime cache and local query engine.
  - Enables high-speed reads and transactions.
  - Ensures consistency before committing to Git.
- **Dual-layer design:** Combines Git’s auditability with SQLite’s responsiveness — ensuring both global consistency and local determinism.

## 4.7 Security and Policy Strategy
**Summary:** Safety is enforced architecturally through sandboxing, policy engines, and cryptographic integrity.

- **Sandboxed Execution:** `rhai` isolates plugin logic; extensions cannot access unsafe APIs.
- **Authorization:** `cedar` provides declarative, policy-based access control.
- **Secrets Management:** `rops` isolates credentials and environment secrets.
- **Integrity and Auditing:** Git’s commit graph ensures non-repudiation and deterministic recovery.
- **Event Isolation:** All plugin communication occurs via typed events; untrusted code never runs inline in the request path.

## 4.8 Evolution and Sustainability Strategy
**Summary:** Domain and infrastructure are decoupled to allow future evolution without violating safety or performance.

Because of the hexagonal pattern:
- Domain logic remains independent of transport, storage, or UI layers.
- Components can evolve individually (e.g., replacing SQLite with a distributed backend).
- New delivery channels (headless API, mobile admin) can be added as adapters.
- Strong typing and static analysis ensure long-term maintainability.

Rust’s stable ecosystem and compile-time guarantees provide sustainability for years to come.

## 4.9 Summary
WhisperCMS’s solution strategy integrates:
- A **hexagonal architecture** with strong isolation boundaries.
- A **priority hierarchy**: Safety → Performance → UX → DX.
- A **zero-cost abstraction** rule across all components.
- **Git-based immutability** and **Rust-based determinism**.
- **Sandboxed, event-driven extensions** for controlled extensibility.
- Deterministic flows and policies ensuring both trust and speed.

**Result:**
A modern CMS engine that is verifiable, extensible, and performant — without the compromises of legacy systems.
