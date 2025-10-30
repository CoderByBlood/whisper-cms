# 09 Architecture Decisions
## 9.1 Overview
**Summary:** Architectural decisions in WhisperCMS are guided by immutable priorities:
1. Safety  
2. Performance  
3. User Experience (UX)  
4. Developer Experience (DX)  

No decision may trade off a higher priority for a lower one. This section records the most significant choices shaping the system’s structure, technologies, and behavior.

### 9.1.1 Summary Table
**Summary:** Architectural decisions prioritize safety, determinism, and long-term sustainability.

| ID | Decision | Core Benefit |
| --- | -------- | ------------ |
| AD-001 | Rust as language | Safety and performance |
| AD-002 | Hexagonal architecture | Isolation and extensibility |
| AD-003 | Git as system-of-record | Traceability and durability |
| AD-004 | SQLite cache | Speed and simplicity |
| AD-005 | Sandboxed extensions | Containment of untrusted code |
| AD-006 | Reactive extension architecture | Predictability and decoupling |
| AD-007 | Desktop admin via Tauri | Secure, offline management |
| AD-008 | Zero-cost abstractions | No hidden overhead |
| AD-009 | Separate plugins/themes | Clear responsibilities |
| AD-010 | Git as sync channel | Deterministic deployment |
| AD-011 | Deterministic flows | Reproducible runtime |
| AD-012 | Declarative policies | Consistent security |
| AD-013 | Local-first design | Resilience and autonomy |
| AD-014 | Untrusted extensions | Safety by design |
| AD-015 | File-based content | Transparency and portability |
| AD-016 | Minijinja templating | Safe, familiar rendering |

**Result:**  
WhisperCMS’s architecture embodies its mission — *to be the safest and fastest CMS* — through deliberate, measurable design decisions that enforce safety and performance at every layer.

## 9.2 AD-001 — Adopt Rust as the Implementation Language
**Context:**  
Past CMSs (e.g., WordPress, Drupal) use dynamically typed languages with shared global state, leading to performance and security issues.

**Decision:**  
Implement WhisperCMS entirely in **Rust**.

**Rationale:**  
- Guarantees **memory safety** and **thread safety** at compile time.  
- Enables **predictable performance** via zero-cost abstractions.  
- Integrates strong async and type systems suited for IO-heavy workloads.  
- Reduces attack surface by eliminating runtime reflection or eval.  

**Consequences:**  
- Higher initial developer onboarding curve.  
- Requires custom plugin sandboxing (`rhai`) for dynamic behavior.  
- Achieves near-native performance and verifiable determinism.

## 9.3 AD-002 — Use Hexagonal (Ports & Adapters) Architecture
**Context:**  
Legacy CMS architectures entangle domain, UI, and infrastructure logic, making safety and testing difficult.

**Decision:**  
Adopt a **hexagonal architecture** with strict separation between:
- Domain Entities  
- Application Services  
- Adapters (UI, CLI, Server)  
- Infrastructure (Git, SQLite, Filesystem)

**Rationale:**  
- Enables isolation of untrusted code.  
- Allows the system to evolve independently across layers.  
- Simplifies testing and reasoning about data flow.  
- Maps directly to WhisperCMS’s mission: *extensible, safe, and verifiable.*

**Consequences:**  
- Additional boilerplate for adapter boundaries.  
- Higher upfront design cost.  
- Strong long-term maintainability and refactor safety.

## 9.4 AD-003 — Git as the Canonical System-of-Record
**Context:**  
Traditional databases provide transactional state but weak audit trails and non-human-readable formats.

**Decision:**  
Use **Git repositories as the primary persistence and synchronization mechanism** for configuration, content, and extensions.

**Rationale:**  
- Git provides **immutability, traceability, and versioning**.  
- Enables **distributed collaboration** without central coordination.  
- Facilitates rollback, auditing, and diff-based review.  
- Serves as the synchronization bridge between desktop and server.  

**Consequences:**  
- Content updates depend on Git operations (commit/push/pull).  
- Slightly slower persistence for small frequent changes.  
- Gains deterministic version control and decentralized deployment.  

## 9.5 AD-004 — SQLite / LibSQL as Runtime Cache
**Context:**  
Git is excellent for durability but inefficient for query-heavy workloads.

**Decision:**  
Employ **SQLite or LibSQL** as the local runtime database for each instance.

**Rationale:**  
- Provides **transactional integrity** and **fast local reads**.  
- Requires no external service — maintaining simplicity and portability.  
- Aligns with “zero-configuration” goals.  
- Serves as a **temporary cache**, not a system of record.  

**Consequences:**  
- Must ensure sync consistency with Git commits.  
- Each instance maintains its own cache; no global database.  
- Enables predictable performance with minimal operational overhead.

## 9.6 AD-005 — Sandbox All Extension Code
**Context:**  
Untrusted plugins in legacy CMSs are a primary source of security breaches.

**Decision:**  
Execute all plugin and theme logic in **sandboxed interpreters** (Rhai).

**Rationale:**  
- Prevents arbitrary filesystem and network access.  
- Ensures **capability-based security** per extension.  
- Supports dynamic behavior without sacrificing safety.  
- Aligns with “no arbitrary code execution” principle.  

**Consequences:**  
- Limited dynamic power compared to fully native code.  
- Requires explicit capability declarations.  
- Guarantees isolation, reproducibility, and containment of untrusted logic.

## 9.7 AD-006 — Use Reactive Extension Architecture
**Context:**
Legacy CMSs rely on untyped global hooks, causing side effects and non-deterministic behavior.

**Decision:**
Adopt a typed, reactive architecture using Leptos reactivity (`leptos_reactive`) to manage communication between core, plugins, and themes.

**Rationale:**
- Enables deterministic, fine-grained reactivity via typed signals and effects.
- Keeps heavy transformations in the host; extensions react using primitives.
- Prevents cyclic dependencies and hidden side effects.
- Simplifies debugging and tracing through dependency tracking.

**Consequences:**
- Plugins and themes react to typed signals, not global events.
- Host maintains the reactive graph per request.
- Slightly steeper learning curve for reactivity model.
- Gains predictability, safety, and performance.

## 9.8 AD-007 — Local-First Administration via Tauri Desktop App
**Context:**  
Traditional CMSs expose web-based admin panels, often insecure or dependent on continuous connectivity.

**Decision:**  
Provide a **Tauri + Svelte desktop application** for site administration instead of a remote admin API.

**Rationale:**  
- Keeps administrative operations **local-first** and offline-capable.  
- Eliminates remote attack surface for site management.  
- Leverages Git for all synchronization.  
- Provides a familiar workflow without exposing privileged APIs.  

**Consequences:**  
- Requires installation of a desktop client.  
- Limits remote collaborative editing without Git integration.  
- Gains strong security posture and consistent UX.

## 9.9 AD-008 — Prioritize Zero-Cost Abstractions
**Context:**  
Layered systems often degrade performance due to unnecessary indirection.

**Decision:**  
Mandate **zero-cost abstractions** across all modules.

**Rationale:**  
- Prevents hidden runtime, memory, or security costs.  
- Encourages explicit design and measurable verification.  
- Aligns with the Rust philosophy of *“you only pay for what you use.”*  

**Consequences:**  
- Requires profiling and justification for every abstraction.  
- Discourages overly generic frameworks or hidden indirection.  
- Results in high performance and easy reasoning about cost.

## 9.10 AD-009 — Separate Plugins and Themes
**Context:**  
Mixing logic and presentation leads to unpredictable interactions and security issues.

**Decision:**  
Strictly separate **plugins (logic)** from **themes (presentation)**.

**Rationale:**  
- Prevents logic injection through templates.  
- Keeps presentation layers predictable and safe.  
- Enables independent lifecycle management.  
- Supports parallel development of UI and business logic extensions.  

**Consequences:**  
- Reduces flexibility for tightly coupled features.  
- Requires dual repositories for complex extensions.  
- Gains strong maintainability and isolation of concerns.

## 9.11 AD-010 — Use Git as Synchronization Channel (Not API)
**Context:**  
Typical headless CMSs synchronize admin and delivery through APIs or message queues, which introduce latency and complexity.

**Decision:**  
Synchronize desktop and server exclusively through **Git push/pull operations**.

**Rationale:**  
- Eliminates runtime network APIs between admin and server.  
- Simplifies deployment and synchronization.  
- Guarantees **traceable, atomic configuration updates**.  
- Works seamlessly with distributed workflows (e.g., self-hosted Git, GitHub).  

**Consequences:**  
- Delayed propagation of changes (requires Git push/pull).  
- No real-time preview unless locally configured.  
- Gains versioned, verifiable state transitions.

## 9.12 AD-011 — Deterministic Startup and Delivery Flows
**Context:**  
Unordered initialization and unbounded hooks cause instability in traditional CMSs.

**Decision:**  
Define **explicit startup and request-delivery sequences** as shown in process diagrams.

**Rationale:**  
- Guarantees reproducibility and safe initialization order.  
- Simplifies reasoning, testing, and debugging.  
- Ensures all extensions registered before first request.  
- Aligns with “local reasoning” and determinism principles.  

**Consequences:**  
- Slightly slower startup due to explicit checks.  
- Reduced flexibility for runtime hot-reloads.  
- Gains full transparency and predictable runtime state.

## 9.13 AD-012 — Use Declarative Policies for Authorization
**Context:**  
Imperative access control is error-prone and inconsistent across modules.

**Decision:**  
Implement **declarative authorization via Cedar** policy engine.

**Rationale:**  
- Centralized, auditable security model.  
- Clear separation of enforcement and logic.  
- Simplifies testing and compliance verification.  
- Enables policy-as-code lifecycle with versioning.  

**Consequences:**  
- Requires learning Cedar syntax for policy authors.  
- Adds runtime policy evaluation step (minimal cost).  
- Gains consistency, traceability, and compliance readiness.

## 9.14 AD-013 — Favor Local-First Design
**Context:**  
Cloud-first architectures depend on continuous connectivity and centralized services.

**Decision:**  
Design WhisperCMS as a **local-first system** where every site is self-contained.

**Rationale:**  
- Enables offline operation and independence from external services.  
- Simplifies hosting and reduces operational risk.  
- Aligns with decentralized, Git-driven philosophy.  

**Consequences:**  
- Synchronization latency between environments.  
- Requires merge conflict handling in Git.  
- Gains resilience, autonomy, and simplicity.

## 9.15 AD-014 — Treat Extensions as Untrusted Code
**Context:**  
Historical CMS compromises stem from unverified third-party code.

**Decision:**  
Treat all extensions as **untrusted until proven safe**.

**Rationale:**  
- Enforces principle of least privilege.  
- Prevents code injection and privilege escalation.  
- Simplifies audit and threat modeling.  

**Consequences:**  
- Restricts extension capabilities by default.  
- Requires manifest review and explicit permission grants.  
- Guarantees systemic safety regardless of plugin source.

## 9.16 AD-015 — Prefer Deterministic File-Based Content
**Context:**  
Dynamic CMSs often rely on mutable databases that lose structure and human readability.

**Decision:**  
Store content in **human-readable formats** (Markdown, YAML, TOML, JSON) within Git.

**Rationale:**  
- Ensures content durability beyond system lifespan.  
- Enables manual inspection, diffs, and merges.  
- Supports external tooling and interoperability.  

**Consequences:**  
- Large sites require incremental syncs.  
- Slightly higher I/O during bulk updates.  
- Gains transparency, portability, and future-proof storage.

## 9.17 AD-016 — Use Templating for Presentation (Minijinja)
**Context:**  
Templating must be safe, performant, and familiar.

**Decision:**  
Adopt **Minijinja** for theme rendering.

**Rationale:**  
- Compatible with Jinja2 syntax (developer familiarity).  
- Sandboxed and precompiled for performance.  
- Safe against injection and template-time execution.  
- Integrates with Rust’s type-safe rendering.  

**Consequences:**  
- Requires pre-defined template contexts.  
- No arbitrary template evaluation at runtime.  
- Gains both performance and safety.
