# 02 Architecture Constraints
## Foundational Principles
This section has two parts that work together but are not the same:
1. Priorities (in strict order) — they govern every decision, and you cannot trade off a higher priority to satisfy a lower one.
2. Cross-cutting constraint: Zero-Cost Abstractions — a standing rule that applies across all priorities.

### The Priorities (non-negotiable order)
1. **Safety:** The system must protect data, access, and integrity above all else. If a proposal improves anything else but weakens safety, it is rejected. **Decision rule:** prefer designs that minimize attack surface, reduce implicit trust, and keep failure modes contained.
2. **Performance:** The system should be responsive and efficient under realistic load without relying on fragile shortcuts. Performance work must never undermine Safety. **Decision rule:** choose designs that make correct, fast behavior the default; measure and remove unnecessary work.
3. **User Experience (UX):** The product must be clear and predictable for non-technical users: create, edit, preview, publish, manage. UX improvements are welcome after Safety and Performance are satisfied. **Decision rule:** optimize workflows and feedback without introducing hidden complexity or risk.
4. **Developer Experience (DX):** The engine should be pleasant to extend and maintain, with predictable APIs and tooling. DX may not compromise UX, Performance, or Safety. **Decision rule:** prefer explicit, typed, and discoverable extension points over “magic.”

> How to use the priorities: When priorities collide, apply them top-down. If a change improves UX but harms Performance, it’s out. If it improves Performance but harms Safety, it’s out immediately. If it improves DX but complicates UX, it’s out.

### Cross-Cutting Constraint: Zero-Cost Abstractions
This constraint applies to every design and feature: an abstraction is only acceptable if it adds no meaningful runtime, memory, or security cost compared to a direct, purpose-built implementation for the same behavior.

What this means in practice
- Pay-for-what-you-use: Optional features must impose near-zero overhead when not used.
- No abstraction tax: Layers, indirection, or generality must not introduce measurable slowdowns, larger attack surfaces, or harder reasoning compared to a direct solution.
- Local reasoning: Abstractions should make behavior easier to reason about, not harder. If an abstraction obscures control flow or error handling, it fails the constraint.
- Measurable verification: Claims of “zero-cost” must be supported by profiling/benchmarks and basic security review.
- Escape hatches: When general solutions risk cost creep, provide targeted, simpler paths that satisfy the constraint.

> How Zero-Cost interacts with priorities: If an abstraction helps DX but adds overhead that harms Performance (Priority #2) or expands the attack surface (Priority #1), it does not pass. If an abstraction helps UX but forces hidden runtime work everywhere, it does not pass.

### How we apply this day-to-day
- Design proposals must include:
  - Which priority they serve,
  - Impact on higher priorities (must be none or clearly mitigated),
  - Evidence for zero-cost (or why cost is negligible and contained).
- Review checklist:
  - Does this weaken Safety in any way? (If yes → reject.)
  - Does this add runtime/memory or widen trust boundaries compared to a direct approach? (If yes → justify or redesign.)
  - If optional, is the overhead truly zero when unused?
  - Is the behavior easy to reason about and test?
- Conflict resolution:
  - When two good designs tie on the target priority, choose the one with less abstraction or fewer new trust assumptions, provided it still meets future needs without hidden cost.
- Anti-patterns (always fail):
  - “Convenience” layers that add global state or dynamic dispatch everywhere.
  - Generic hooks that require string lookups where typed, local contracts suffice.
  - Features that are on by default and costly to disable.
  - Cross-cutting mechanisms that complicate error handling or rollback.

WhisperCMS is guided by Priorities—Safety → Performance → UX → DX—that may not be traded off out of order. Over all of it sits Zero-Cost Abstractions: no feature or layer is acceptable if it meaningfully costs more (in speed, memory, or risk) than a direct, purpose-built implementation. This combination keeps the engine safe, fast, pleasant to use, and sustainable to build on.
