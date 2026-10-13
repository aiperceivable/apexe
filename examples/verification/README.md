# Verify explicit cases and invalidate stale evidence

This self-contained example creates two synthetic CLIs, bindings, an ACL and
cases in a temporary directory. One exercises explicit dry-run authorization;
the other checks isolated fixture writes. Their source overlays are `low`
confidence. Successful concrete cases earn `verified` evidence tied to digests.
No destructive command or external corpus is required.

```bash
cargo build --all-features
python3 examples/verification/demo.py --apexe target/debug/apexe
```

The demo checks untouched original bindings, successful dry-run/isolated cases,
recorded confidence, and automatic invalidation after case-file or executable
changes. It uses `apexe list --verbose --format json` to inspect module metadata.
All files are discarded afterward.

macOS uses system `sandbox-exec`; Linux needs `bwrap` and enabled unprivileged
user namespaces. If required isolation is unavailable, the demo reports
`supported: false` with a reason and executes no behavioral case. This does not
count as a passed verification of either CLI. Windows has no backend in this
release. Read the [verification guide](../../docs/verification.md) for the case
format, admission rules and evidence scope.
