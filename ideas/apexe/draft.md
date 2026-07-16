# apexe

> Status: Parked (NOT VALIDATED as product) | Draft v1 | 2026-06-04

## One-Liner
Auto-parse any CLI's `--help` into governed MCP tools for AI agents.

## Problem
Agents need to invoke CLI tools safely. **But** this is already served by (a) generic bash/shell
tools every agent platform ships, and (b) a crowded field of CLI→MCP wrappers; the *governance*
sub-problem is being solved one layer up by control-plane gateways. apexe targets a real problem
at the wrong altitude.

## Target Users
Undefined. The sole maintainer does not use it; no inbound demand; no named early adopters.

## Core Concept
Three-tier deterministic scan (--help → man → completions) → JSON Schema → governed MCP tools
(ACL, approval, audit, annotations), built on the apcore ecosystem.

## Existing Solutions & Gaps
- **CLI→MCP wrapping (commoditized):** avelino/mcp (single-binary --help→MCP), any-cli-mcp-server,
  CLI Wrapper (Zod), mcptools, mcp2cli, cmd-line-mcp, shell-command-mcp. No gap.
- **Governance (out-positioned):** AGT (client↔server policy layer), Cerbos, Microsoft/AWS/Strata/
  Palo Alto control planes. A per-CLI wrapper governs less than a gateway. No gap.
- **Only un-served angle:** apcore's multi-surface fan-out (wrap once → MCP+A2A+CLI+REST + one
  governance model). Competitors are all MCP-only. This is a *demo* angle, not a product wedge.

## MVP Scope
N/A as a product. If kept as an **apcore demo**: smallest version = 1–2 CLIs, governance + the
multi-surface fan-out front-and-center; do NOT port the full 9KLOC parser zoo.

## Demand Validation Status
- [x] Competitive analysis done — see research/competitors.md
- [x] "What if we don't build this?" answered — **Low impact** (pseudo-requirement signal)
- [ ] Problem backed by demand evidence — **fails** (no users, incl. creator)
- [ ] Target users identified & reachable — **fails** (none named)
- [ ] Differentiation clear — **fails** (commoditized A-axis; out-positioned B-axis)

## Verdict
**NOT VALIDATED as a standalone product.** Do not product-ize; do not faithfully rewrite.
Residual legitimate role: **apcore "Outside-In" demo**, judged by whether it makes apcore
credible — and even then, framed around the multi-surface+governance angle, not "CLI→MCP".
Flip condition: concrete inbound demand for batch, governed exposure of *internal/private* CLIs.

## Session History
- [2026-06-04] validate-demand — NOT VALIDATED; commoditized + out-positioned + no users.
