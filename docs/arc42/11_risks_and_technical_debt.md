# 11 Risks and Technical Debt
## 11.1 Overview
**Summary:**
WhisperCMS is intentionally designed to minimize systemic risk through deterministic, sandboxed, and version-controlled architecture.
However, any evolving system faces implementation, integration, and ecosystem risks that must be explicitly managed.

This section outlines:
- Architectural risks (strategic, systemic, or technological),
- Technical debts (known trade-offs accepted for progress),
- Mitigation and monitoring strategies.

## 11.2 Architectural Risks

| ID | Risk | Description | Potential Impact | Mitigation |
| --- | ---- | ----------- | ---------------- | ---------- |
| **R-01** | **Sandbox Escape** | Untrusted plugin gains access outside its declared capabilities. | Compromised system integrity or data leak. | Enforce Rhai capability whitelisting; fuzz and penetration test every release; integrate static sandbox verifier. |
| **R-02** | **Git Synchronization Conflict** | Concurrent admin commits create merge conflicts or inconsistent server state. | Site may serve outdated content or require manual resolution. | Enforce branch-based workflow (`admin`, `preview`, `main`); detect conflicts on server pull; auto-rollback to last known valid commit. |
| **R-03** | **Event Bus Overload** | Too many concurrent plugin events cause delays. | Latency spikes or blocked requests. | Asynchronous event execution; per-plugin timeouts; load metrics and backpressure using Tokio channels. |
| **R-04** | **Performance Degradation in Large Sites** | SQLite indexing or Git commit overhead grows with site size. | Slow startup or high CPU under load. | Incremental content ingestion; background reindexing; evaluate LibSQL distributed mode for large deployments. |
| **R-05** | **Configuration Drift** | Admin modifies local settings without committing to Git. | Unsynchronized or unreproducible environments. | Enforce commit-before-restart; auto-detect local changes; warn user before unsynced shutdown. |
| **R-06** | **Dependency Supply Chain Vulnerabilities** | Crate ecosystem dependency introduces security flaw. | System compromise or crash. | Continuous `cargo-audit` scans; pinned crate versions; vendor critical crates when necessary. |
| **R-07** | **Policy Misconfiguration (Cedar)** | Incorrect authorization policy blocks or permits unintended access. | Security breach or loss of service. | Ship policy validator CLI; provide sample templates and pre-flight checks. |
| **R-08** | **Git Repository Corruption** | Unexpected OS or filesystem failures corrupt local Git repo. | Startup failure or data loss. | Daily backup; verify repo integrity on startup; rebuild from remote origin if checksum mismatch. |
| **R-09** | **Desktop / Server Divergence** | Differences in version or schema between admin and server. | Incompatible commits or failed syncs. | Version negotiation via manifest; pre-push validation; schema migration scripts. |
| **R-10** | **Complexity of Cross-Platform Builds** | Maintaining Tauri app across macOS, Windows, Linux. | Release delays or inconsistent UI behavior. | Automated CI build matrix; nightly cross-platform testing. |

## 11.3 Technical Debts (Accepted Trade-offs)

| ID | Technical Debt | Justification | Impact | Planned Resolution |
| --- | -------------- | ------------- | ------ | ------------------ |
| **TD-01** | **Limited Hot-Reloading of Extensions** | Safety-first design forbids dynamic loading post-startup. | Slower developer iteration. | Future safe reload protocol using precompiled sandbox cache. |
| **TD-02** | **Rhai VM Overhead** | Each sandbox initialization adds small startup cost. | Slightly longer plugin load times. | Pool and reuse Rhai VMs per extension with scope reset. |
| **TD-03** | **Manual Merge Resolution** | Git remains human-managed for conflict resolution. | User training required. | Build guided merge UI in desktop admin (future release). |
| **TD-04** | **Limited Real-Time Collaboration** | No API synchronization by design. | Users can’t co-edit simultaneously. | Optional extension layer for real-time preview (never core). |
| **TD-05** | **High Binary Size** | Static linking of Rust + dependencies (~50–70 MB). | Larger distribution footprint. | Optimize builds with LTO and feature gating per target. |
| **TD-06** | **Lack of Full i18n Framework** | Locale independence prioritized. | No built-in translation tooling. | Implement as optional extension after v1.0. |
| **TD-07** | **Partial Marketplace Integration** | Marketplace logic external to core. | Manual discovery for extensions. | Gradual integration through federated Git registries. |
| **TD-08** | **Limited Observability UI** | `tracing` exposes telemetry but no dashboard. | Requires external log parsing. | Build optional “Tracing Explorer” desktop module. |
| **TD-09** | **Single-Site SQLite Cache** | Multi-site setups each maintain local caches. | Duplicate computation. | Investigate LibSQL networked mode or sharding. |
| **TD-10** | **Policy Authoring Complexity** | Cedar requires domain-specific syntax. | Learning curve for admins. | Provide UI-driven policy builder in desktop app. |

## 11.4 Strategic Risks
**Summary:** Broader systemic or organizational risks.

| Risk | Description | Mitigation |
| ---- | ----------- | ---------- |
| **Ecosystem Maturity** | Rust CMS ecosystem smaller than PHP/JS equivalents. | Contribute core crates to open source; document APIs extensively. |
| **Adoption Barrier** | Rust toolchain unfamiliar to web developers. | Provide precompiled binaries and extension templates. |
| **Market Perception** | “Developer-centric” positioning may deter non-technical users. | Maintain WordPress-parity UX while emphasizing safety. |
| **Long-Term Maintenance** | Limited contributors could slow updates. | Modular architecture enables community extension development. |

## 11.5 Monitoring and Review
**Summary:** WhisperCMS maintains a proactive risk management cycle.

- **Risk Review Cadence:** quarterly architectural risk audit.
- **CI Checks:** security scanning (`cargo-audit`, `cargo-deny`), coverage, performance regression benchmarks.
- **Release Gate:** no new release unless all critical risks rated ≤ Medium.
- **Incident Logging:** structured event telemetry sent to optional remote endpoint.
- **Recovery Procedures:** documented Git rollback and cache rebuild steps for every deployment.

## 11.6 Risk Severity Matrix

| Severity | Description | Response Strategy |
| -------- | ----------- | ----------------- |
| **Critical** | Compromises safety or integrity. | Immediate fix; block release. |
| **High** | Affects performance or reliability. | Fix in next sprint; monitor. |
| **Medium** | Impacts UX or DX but not safety. | Schedule remediation. |
| **Low** | Cosmetic or documentation issue. | Backlog improvement item. |

## 11.7 Summary
**Summary:**
WhisperCMS’s major risks stem from its security-first isolation model and distributed synchronization design.
Its mitigations — sandboxing, version control, declarative policies, and deterministic flows — ensure that no single fault can compromise system safety.

**Result:**
The architecture remains resilient against untrusted code, synchronization drift, and scaling challenges while explicitly tracking known debts for structured resolution.
