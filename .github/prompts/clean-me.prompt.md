
## Goal

Improve maintainability, readability, correctness, and consistency while preserving observable behavior.

Remove dead or obsolete code, eliminate duplication, simplify design, strengthen types, and keep the repository aligned with its documentation, tests, schemas, and runtime behavior.

---

## Tasks

* Delete dead code (unused, unreferenced, stale, or obsolete).
* Remove obsolete paths, flags, configurations, and duplicate implementations.
* Synchronize documentation with actual APIs, configuration, and behavior.
* Detect contradictions between:

  * Code ↔ Documentation
  * Code ↔ Tests
  * API ↔ Implementation
  * Schema ↔ Types (schema/type drift)
  * Configuration ↔ Runtime
* Detect unreachable, redundant, or contradictory logic.
* Improve maintainability through incremental refactoring.

---

## Refactoring Focus

Look for opportunities to improve:

* Long or complex functions
* Duplicated logic
* Unclear naming
* Weak or missing types
* Large or tangled modules
* High coupling
* Deep nesting
* Brittle conditionals
* Mixed orchestration and business logic

Prefer:

* Extract function
* Extract object or module
* Rename for domain clarity
* Remove duplication
* Split orchestration from pure logic
* Tighten types and invariants
* Isolate side effects
* Simplify control flow

---

## Workflow

1. Identify behavior that must not change.
2. Find or add focused tests before risky edits when practical.
3. Make one small, incremental refactor at a time.
4. Run targeted verification after each meaningful step.
5. Stop if behavior changes or verification becomes uncertain.
6. Summarize structural improvements and the evidence supporting correctness.

---

## Output

### Safe Removals

For each removal include:

* Item
* Reason
* Confidence (High / Medium / Low)

### Suggested Diffs

Provide minimal, behavior-preserving changes.

### Contradictions

| Type | Location | Issue | Severity | Recommended Fix |
| ---- | -------- | ----- | -------- | --------------- |

### Risks

Highlight anything requiring manual review before merging.

---

## General Rules

* Preserve observable behavior.
* Do not mix refactoring with feature development.
* Do not rewrite from scratch unless explicitly requested.
* Preserve public APIs unless a deprecation plan is included.
* Do not remove public APIs without deprecation.
* Avoid false positives caused by reflection, code generation, plugins, macros, or dynamic loading.
* Keep changes incremental, reversible, and easy to review.
* Auto-fix only when the change is behavior-preserving and confidence is high.
* If tests are absent, reduce scope and verify behavior carefully.

---

## Rust (apply only to Rust code)

### Goals

Produce safe, idiomatic Rust that satisfies both the borrow checker and the test suite.

### Prefer

* Model domains with enums and structs.
* Make invalid states unrepresentable.
* Return `Result` with typed errors.
* Reserve panics for bugs or tests.
* Borrow by default; clone only with a documented reason.
* Prefer iterators and combinators over manual indexing.
* Encode invariants in the type system.

### Idioms

* Use `?` for error propagation.
* Use `Option` and `Result` combinators (`map`, `and_then`, `ok_or`, etc.).
* Prefer `thiserror` for libraries.
* Prefer `anyhow` for applications.
* Use traits for shared behavior.
* Use generics with appropriate bounds.
* Prefer `&str` and `&[T]` over owned parameters where appropriate.

### Verification

Keep the project clean under:

* `cargo fmt`
* `cargo clippy`
* `cargo test`

### Rust Rules

* Avoid `unwrap()` and `expect()` outside tests or proven invariants.
* Avoid unnecessary `clone()`.
* Do not introduce `unsafe` without documented justification.
* Let the borrow checker guide the design instead of fighting it.
* Treat Clippy warnings as actionable signals.
