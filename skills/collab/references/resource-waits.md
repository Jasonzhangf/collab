# Resource Coordination and Waits

Before mutating a shared feature, resource, mainline node, gate, integration
lease, install target, or daemon:

1. Refresh durable state and attempt the semantic claim.
2. If occupied, persist the requester as blocked and return holder/responsible
   actor synchronously.
3. Send `RESOURCE_OCCUPIED` only when coordination is needed.
4. Create a bounded wait only for a real blocker/responsible actor.
5. Subscribe to the exact release/deadline event only when async wake is useful.

```sh
collab sendmessage --to <peer> --subject resource-busy "RESOURCE_OCCUPIED ..."
collab sendmessage --to <peer> --subject resource-free "RESOURCE_RELEASED ..."
collab sendmessage --to <peer> --subject result-ready "The result is ready; query the mailbox."
```

Every wait records waiter, exact blocking task, responsible actor,
`resource_conflict`, finite deadline, resume events, and escalation path.

The server rejects self-wait, missing owner, unrelated resource, terminal or
delivered waits, missing deadline/resume path, and direct/transitive cycles.
Timeout makes the waiter explicitly blocked and never sends a message or
releases a claim. Holder close clears obsolete wait edges and creates a
`RESOURCE_RELEASED` notification only for an exact matching subscription.

Waiting is not abandonment. On each 15-minute task-liveness check, the waiter
must re-read the durable conflict, try any locally available resolution, and
escalate unresolved work. A managed subagent and an ordinary worker both
escalate to the live Collab master immediately after finding a concrete
solution; they do not wait or dump symptoms. A subagent also copies its
parent when parent is not the master. If no live master exists, escalate to
the task-initiating collaborator. Independent peers may decline a master
collaboration invite. Include the blocking task, responsible actor,
deadline, proposed solution, attempted actions, and requested decision. Never
invent a master from `appsdk init`; master promotion requires explicit user
approval for the exact peer and project plus live identity verification. If a
live master exists, only that master may delegate; if none exists, a peer may
promote itself after approval. Codex/Cursor root is not Collab master.
