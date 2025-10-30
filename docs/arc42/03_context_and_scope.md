# 03 Context and Scope
## Extensions: Plugins and Themes
Extensibility is central to WhisperCMS, but never at the expense of security. It organizes add-ons into the category of extensions, which come in two types: plugins and themes. Extensions are architected with enforced boundaries that prevent them from compromising security or system integrity.
- Plugins extend and modify functionality without altering how content is presented.
- Themes define presentation and user-facing design.
- Boundaries are enforced so that extensions cannot bleed across responsibilities. Plugins do not silently take over theming, and themes stay focused on delivery.

This separation is central to keeping the system predictable and safe, while still giving developers powerful ways to extend behavior.

## Git: Distributed Version Control
WhisperCMS uses Git as its canonical source of truth for all configuration, content, and versioned state. Every site instance is backed by a dedicated Git repository that serves as the synchronization and audit boundary between administrators, extensions, and runtime components. Within this context, Git provides immutable version history, branch-based isolation (e.g., main for production, admin for staging and preview), and a uniform transport for configuration changes.

The system leverages Git not merely as a developer tool but as a distributed content ledger: each commit represents a deterministic snapshot of the system’s state, ensuring traceability, reversibility, and verifiability across environments. Git therefore defines the outer boundary of the system’s persistence domain—no data mutation is considered durable until it is persisted to and verifiably recorded in the corresponding Git branch.

## SQLite & LibSQL: Local Performant Persistence
WhisperCMS employs SQLite (or its network-capable variant LibSQL) as the local execution store for fast, isolated, and transactional state management. SQLite/LibSQL provides a zero-configuration, file-backed database engine that aligns with WhisperCMS’s emphasis on simplicity, performance, and determinism. It is not used as a system-wide database service, but rather as a scoped persistence layer whose lifecycle is tied to the corresponding site or process.

Git remains the system-of-record, while SQLite/LibSQL acts as the runtime cache and query engine that enables efficient reads, content indexing, and transactional validation before promotion to Git. This dual-layer approach ensures local responsiveness and global consistency without introducing external dependencies or operational complexity.

## Functionally Out of Scope
The WhisperCMS Architecture Specification defines the core functional boundaries of the platform — what WhisperCMS must do to operate safely, extensibly, and predictably. However, it also delineates a large body of functionality that is explicitly excluded from the platform’s core responsibilities. These out-of-scope functions may exist in the broader ecosystem (as extensions, companion tools, or integrations) but are not part of the WhisperCMS core or its architectural guarantees.
### 1. Marketing, Commerce, and Membership Features
WhisperCMS is a content management system, not a digital experience or e-commerce platform. Features such as cart management, checkout, payments, membership tiers, or subscription handling are intentionally excluded. These can be implemented as extensions or external integrations but will never be part of the core system.
### 2. Editorial Workflow Automation
While the architecture supports drafting, publishing, and versioning, it does not implement full editorial workflows such as multi-step approvals, content assignment, or task automation. These belong to higher-level product layers or third-party extensions. The system provides the primitives (states, versioning, events) but not the workflow logic itself.
### 3.  Media Asset Management
WhisperCMS supports references to static assets (e.g., images, documents, media) but does not include full Digital Asset Management (DAM) capabilities such as transcoding, resizing, tagging, or metadata enrichment. These functions should be handled by extensions or connected external services.
### 4. Advanced Recommendation Engines
The architecture provides structured content and queryable metadata include full-text search engines, semantic ranking, and basic personalization but does not define or include advanced personalization or AI-assisted content recommendation. Implementations may add these capabilities through external search backends or specialized extensions.
### 5. Analytics, Reporting, and Dashboards
Observability and metrics are supported at the operational level including site traffic reports, but not the business intelligence level. WhisperCMS does not produce editorial analytics or engagement metrics. These are integration surfaces for external tools, not core platform functions.
### 6.  Localization, Internationalization, and Multilingual Workflows
The architecture is language-agnostic and locale-independent. It defines no conventions for translating or versioning content across locales. Multi-language publishing can be implemented through content structures or extensions, but it is not a core concern of the platform.
### 7.  User Management Beyond Administrative Access
Only administrative and system-level identities are within architectural scope. End-user authentication, registration, or profile management (e.g., “site visitors” or “members”) are out of scope. Extensions can add authentication layers or integrate with identity providers, but the core system remains focused on admin, machine, and extension identities.
### 8. SEO, Marketing Optimization, and Social Integrations
WhisperCMS does not directly implement SEO tools, meta tag generators, or social network publishing APIs. The system provides structural clarity and routing predictability so these can be layered on externally or via extensions.
### 9. Live Editing Interfaces
While the architecture supports themes and content administration, it does not define an interactive live editor, drag-and-drop builder, or visual composer. Those are considered user experience layers that may be built atop the architecture but are not part of it.
### 10. AI-Assisted Authoring or Automation
WhisperCMS is neutral toward artificial intelligence or automation tools. It does not include AI-based content generation, tagging, or moderation. Those capabilities can be introduced via extensions under the same isolation and capability constraints as all other untrusted logic.
### 11. External API Aggregation or Federated Content Sources
WhisperCMS does not act as a gateway or integrator for external APIs, content feeds, or third-party data sources. While extensions may fetch and transform external data, the core system itself does not manage synchronization, federation, or remote data pipelines.
### 12. Workflow Orchestration Across Sites or Tenants
Multi-tenancy ensures isolation, not coordination. WhisperCMS does not provide tools for synchronizing or managing multiple tenants, sites, or environments in a unified workflow. Each tenant or instance is operationally autonomous.

### Summary
Functionally, WhisperCMS defines how content, configuration, and extension logic are structured, isolated, and governed — but it deliberately avoids taking a position on what business functions those capabilities enable. It provides a secure, extensible substrate for any content-centric domain (blogs, docs, marketing, internal systems) without embedding assumptions about commerce, marketing, analytics, or editorial workflows.

Put simply:
> WhisperCMS is not a product suite — it is an architectural core for safely running and composing content-driven systems.
