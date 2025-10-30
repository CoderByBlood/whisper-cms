# 12 Glossary
**Purpose:**  
This glossary defines important terms and concepts used throughout WhisperCMS architecture, ensuring consistency and clarity for all contributors.

## 12.1 Core Concepts

| Term | Definition |
| ---- | ---------- |
| **WhisperCMS** | A modern, Rust-based content management engine designed with a *Safety → Performance → UX → DX* priority model. Its goal is to be the safest and fastest CMS by enforcing strong boundaries and deterministic behavior. |
| **Content Engine** | The foundational runtime that models, stores, and publishes structured content. It is not a framework or page builder but a composable architecture for content-driven systems. |
| **Zero-Cost Abstractions** | A design rule stating that abstractions are only acceptable if they add negligible runtime, memory, or security cost compared to a direct implementation. |
| **Priority Model** | The guiding hierarchy for all design decisions: **1) Safety → 2) Performance → 3) User Experience → 4) Developer Experience**. No lower priority may override a higher one. |
| **Hexagonal Architecture** | Also known as Ports & Adapters; separates domain logic from infrastructure and user interfaces, enabling safe, testable, and evolvable boundaries. |
| **Domain Layer** | The inner layer defining core business rules, content models, and policies, independent of delivery or infrastructure. |
| **Adapter Layer** | Interfaces connecting the domain to the outside world — includes web routes, CLI commands, plugins, and desktop UI interactions. |
| **Infrastructure Layer** | Provides persistence, network, filesystem, and framework integrations (e.g., Git, SQLite, Pingora, Axum). |

## 12.2 Persistence and Data Terms

| Term | Definition |
| ---- | ---------- |
| **Git Repository** | Canonical system-of-record storing configuration, content, and extension manifests. Provides audit trails, branching, and rollback. |
| **SQLite / LibSQL** | Embedded transactional databases providing fast local caching and indexing for runtime operations. |
| **System-of-Record** | The authoritative data source — in WhisperCMS, always Git. No data is considered durable until committed. |
| **Commit** | A verifiable change recorded in Git that captures a snapshot of content and configuration state. |
| **Branch** | A named timeline of commits (e.g., *main*, *admin*, *preview*) representing different environments or lifecycles. |
| **Configuration Files** | Human-readable TOML/YAML files (`settings.toml`, `config.toml`) defining instance behavior and extension state. |

## 12.3 Execution and Runtime Concepts

| Term | Definition |
| ---- | ---------- |
| **Server Runtime** | The deployed instance that hosts the Axum web server, Pingora ingress controller, and extension sandboxes to serve public content. |
| **Desktop Runtime** | The Tauri + Leptos desktop admin app for local-first configuration, content management, and publishing via Git. |
| **CLI** | Command-line interface for administration, initialization, and maintenance tasks. |
| **Ingress Controller (Pingora)** | Edge layer validating HTTP requests before they reach the web server; enforces rate limits, allowed methods, and safety checks. |
| **Web Server (Axum)** | Asynchronous HTTP framework serving routes mapped to Themes and dispatching events to Plugins. |
| **Startup Flow** | Deterministic initialization sequence: read configuration → clone Git repo → populate SQLite → register extensions → start server. |
| **Delivery Flow** | Request-handling lifecycle: ingress validation → theme route match → plugin events → template rendering → response streaming. |
| **Sandbox** | Isolated execution context for untrusted plugin or theme code using the `rhai` scripting engine. |

## 12.4 Extensions and Event System

| Term | Definition |
| ---- | ---------- |
| **Extension** | External module extending or modifying system behavior — either a Plugin (logic) or Theme (presentation). |
| **Plugin** | An extension that processes content or alters logic by subscribing to structured events through the Event Bus. |
| **Theme** | An extension controlling presentation, routing, and rendering templates using Minijinja. Themes cannot change core logic. |
| **Extension Host** | Component managing sandbox lifecycle, capability enforcement, and inter-extension communication. |
| **Capability** | A declared permission allowing limited access (e.g., filesystem read, HTTP fetch). Managed through manifest and sandbox policy. |
| **Manifest** | A metadata file describing extension type, capabilities, dependencies, and version compatibility. |
| **Event Bus** | The central mechanism (`leptos_reactive`) for communication between core and extensions. Implements a reactive signal/effect model. |
| **Event Type** | One of the standardized event categories: Request, Content, Response, or System. |
| **Signal** | A signal is a reactive value that notifies dependents when it changes. |

## 12.5 Security and Policy Concepts

| Term | Definition |
| ---- | ---------- |
| **Cedar** | Policy engine providing declarative authorization evaluation (policy-as-code). |
| **ROPS** | Secrets-management crate handling encryption, key storage, and secret distribution. |
| **Tower-Sessions** | Middleware providing HMAC-signed session cookies for authenticated sessions. |
| **Policy** | A declarative rule controlling what identities can perform which actions under which conditions. |
| **Identity** | A verified actor — admin user, system process, or extension instance. |
| **Authorization** | The process of verifying whether an identity may perform an action, enforced by Cedar. |
| **Ingress Boundary** | Network layer boundary that filters and validates all external requests. |
| **Extension Boundary** | Sandbox boundary ensuring no untrusted code escapes its declared capability scope. |

## 12.6 User and Workflow Concepts

| Term | Definition |
| ---- | ---------- |
| **Administrator** | Trusted user managing configuration, themes, and plugins through the desktop admin console. |
| **Site Visitor** | End-user accessing published web content from the server. |
| **Admin Console** | Desktop UI providing configuration, plugin management, and theme control. |
| **Marketplace** | Optional external Git repository indexing publicly available extensions. |
| **Local-First** | Design approach prioritizing offline functionality with deferred Git synchronization. |
| **Preview Branch** | Temporary Git branch used to test configuration before publishing to main. |

## 12.7 Observability and Maintenance Terms

| Term | Definition |
| ---- | ---------- |
| **Tracing** | Structured logging and span tracking mechanism providing runtime observability. |
| **Span** | A timed unit of work recorded for performance or debugging analysis. |
| **Telemetry** | Aggregated metrics (requests, latency, plugin duration) collected from tracing layers. |
| **Snafu** | Rust error-handling crate used for typed, contextual error propagation. |
| **Failsafe / Tower-Resilience-Circuitbreaker** | Libraries ensuring runtime fault isolation and retry control. |
| **Audit Trail** | Historical record of all state changes via Git commits and logs. |

## 12.8 Architectural Boundaries

| Boundary | Description |
| -------- | ----------- |
| **Git Boundary** | Separates admin and server runtimes; all synchronization occurs through Git commits, never direct API calls. |
| **Ingress Boundary** | Separates external HTTP world from the trusted server runtime. |
| **Extension Boundary** | Isolates untrusted plugin and theme execution in sandboxes. |
| **Desktop Boundary** | Enforces process and API separation between Svelte UI and Rust backend in Tauri. |
| **Secret Boundary** | Restricts sensitive data (keys, tokens) to trusted components only. |

## 12.9 Acronyms and Abbreviations

| Acronym | Meaning |
| ------- | ------- |
| **CMS** | Content Management System |
| **DX** | Developer Experience |
| **UX** | User Experience |
| **I18N / L10N** | Internationalization / Localization |
| **VM** | Virtual Machine (used for Rhai sandbox execution) |
| **CLI** | Command-Line Interface |
| **API** | Application Programming Interface |
| **TOML** | Tom’s Obvious, Minimal Language (configuration format) |
| **YAML** | Yet Another Markup Language |
| **JSON** | JavaScript Object Notation |
| **HTTP** | HyperText Transfer Protocol |
| **HMAC** | Hash-based Message Authentication Code |

## 12.10 Summary
**Summary:**  
The glossary consolidates WhisperCMS terminology across architecture, implementation, and user domains.  
Consistent use of these terms ensures clarity in design discussions, documentation, and code reviews.

**Result:**  
A unified vocabulary that reinforces WhisperCMS’s identity as a *secure, deterministic, and extensible content engine* built around explicit boundaries and verifiable behavior.