# Session 1 — 2026-06-04
## Type: Validate

## Context
apexe ("Outside-In CLI→Agent Bridge", ~9KLOC Rust, built on apcore) was up for a TypeScript
rewrite. Before investing, the maintainer asked the harder question: **is there genuine demand
for apexe as a standalone product, or is its only real value as an apcore demo?** Explore and
Research were effectively completed in prior discussion + a focused web competitive scan; this
session runs the Demand Validation Checklist directly. Maintainer asked for brutal candor.

## Demand Validation Checklist

### Check 1 — Problem evidence: MODERATE→WEAK (for apexe's specific framing)
"Agents need to use CLIs" is real, but already solved by giving agents a **bash/shell tool**
(Claude Code, Cursor, Codex). The narrower "govern agent CLI use" problem is real (heavy
vendor activity), but the evidence shows it's solved one layer **up** (control-plane gateways),
not via per-CLI MCP wrappers. So apexe targets a real problem at the **wrong altitude**.

### Check 2 — "What if we don't build this?": LOW IMPACT → RED FLAG
Nothing significant happens. Users have: (a) bash tools, (b) ~8 existing CLI→MCP wrappers
(avelino/mcp, any-cli-mcp-server, CLI Wrapper, mcptools…), (c) control-plane governance
(AGT, Cerbos, MS/AWS/Strata/Palo Alto) for the safety angle. The classic pseudo-requirement tell.

### Check 3 — Target user reality: VAGUE / NONE
Maintainer (sole) says "I don't use it myself." No named early adopters, no inbound demand.

### Check 4 — Differentiation: UNCLEAR
vs simpler wrappers (avelino/mcp has the same single-binary --help pitch) apexe isn't simpler.
vs governance layers, a per-CLI wrapper governs less than a gateway. The "apcore governance
baked in" angle is differentiation **only if you're already on apcore** — which is circular
(apexe exists to sell apcore). The one genuinely unique property is apcore's **multi-surface
fan-out** (wrap once → MCP+A2A+CLI+REST), which all MCP-only competitors lack.

### Check 5 — Simplicity: PASS (but not unique)
One-sentence pitch is clear — and identical to the existing tools'.

## Validation Verdict

```
Problem evidence:    Moderate→Weak (right problem, wrong altitude)
"Not build" impact:  Low  ← pseudo-requirement signal
Target users:        Vague/none (creator doesn't use it)
Differentiation:     Unclear (commoditized A-axis; out-positioned B-axis)
Simplicity:          Pass (but not unique)

Overall: NOT VALIDATED  — as a standalone product.
```

## Decisions
- Do **not** product-ize apexe; do **not** do a faithful 9KLOC TS rewrite for users.
- apexe's legitimate residual role = **apcore "Outside-In" demo**, valued by whether it makes
  apcore credible, not by its own users.
- If kept as a demo, frame it around the ONLY non-commoditized angle: **wrap once → govern →
  expose to ALL surfaces (MCP + A2A + CLI + REST)** — apcore's thesis — not "CLI→MCP" (solved).
- Reconsider even a small TS demo until apexe has a concrete job (e.g., apcore onboarding example).

## Open Questions
- Does the maintainer have any inbound signal (someone asking for batch, governed exposure of
  *internal/private* CLIs)? That is the only fact that would flip this to NEEDS-WORK.

## Raw Notes
Competitive scan recorded in research/competitors.md. Two axes both lost: A (CLI→MCP) is
commoditized (~8 tools); B (governance) is owned at the control-plane level by larger players.
