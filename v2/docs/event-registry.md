# v2 event registry — v2-design-freeze-1

All cross-module and cross-container control flows use versioned envelopes.

Required envelope fields:

```text
event_id, sequence, schema_version, producer, timestamp,
correlation_id, causation_id, payload
```

Initial event families:

```text
IdentityRegistered, IdentityRecovered, ClaimCreated, ClaimReclaimed,
TaskBlocked, TaskDelivered, TaskMerged, TaskClosed,
ResourceConflictDetected, ResourceReleased, WorkerStateObserved,
WakeRequested, DeliverySucceeded, DeliveryFailed, PluginHealthChanged
```

Routing, retry, provider, debug, health, snapshot, wake, and stop semantics
remain side-channel fields and never enter business request/response payloads.
