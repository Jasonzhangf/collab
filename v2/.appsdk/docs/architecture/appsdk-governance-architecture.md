# AppSDK Governance Architecture

## Canonical ownership

```text
contracts/     authoring contracts; source of truth
src/           governance/compiler implementation
templates/     initial project skeletons
playground/    mutable experiment source; never runtime input
active/lib/    immutable current consumable library
protected/     frozen source, contracts, and historical versions
generated/     compiler output and indexes; read-only
```

`contracts/` is authoritative for machine rules. `docs/design/` explains intent. Generated surfaces must identify their source contract and generator. Runtime projects consume `active/lib/**`, not Playground or arbitrary source directories.

## Control flow

```text
Claim + issue
  -> scope admission
  -> Playground experiment
  -> evidence
  -> architecture review
  -> architecture review PASS
  -> mainline source merge
  -> compile Active library
  -> protected source/contracts
  -> controlled verify and Git/artifact lock
  -> closeout
```

## Invariants

1. One feature and one resource have one formal owner.
2. Playground is mutable experiment source and cannot be imported by runtime or release builds.
3. Active is immutable consumable library, not source.
4. Promotion is the only Playground-to-mainline-to-Active path.
5. Protected source/contracts and old Active versions are immutable history.
6. Generated contains compiler output and indexes only.
7. Semantic changes require a new library version and new Active hash.
8. Retired code cannot be a fallback, shadow writer, or hidden runtime dependency.
9. Logical path checks must be paired with host physical isolation.
10. Review and verification inspect the actual mainline diff and generated Active library.
11. A compiled-or-later lifecycle requires a matching, hash-valid artifact.
12. Lock evidence requires Git clean, source commit/tag, library hash, public API hash, review PASS, and immutable previous Active.

## Visibility boundary

Protected + Git provide traceability, change detection, and recovery. They do not prevent a generic agent sharing the repository shell from reading Protected source. True read prevention requires a separate worktree/container or host mount policy.

## RouteCodex adapter boundary

RouteCodex will later provide a project adapter containing its resource, function, mainline, verification, and runtime contracts. AppSDK must not absorb RouteCodex protocol, provider, continuation, tool-governance, or pipeline node semantics.
