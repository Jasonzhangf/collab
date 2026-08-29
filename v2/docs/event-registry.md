# v2 event registry — design-baseline-draft

All cross-module and cross-container control flows use versioned envelopes.

Required envelope fields:

```text
event_id, sequence, schema_version, producer, timestamp,
correlation_id, causation_id, event_type, control, business_payload
```

Initial event families:

```text
IdentityRegistered, IdentityRecovered, ClaimCreated, ClaimReclaimed,
TaskBlocked, TaskDelivered, TaskMerged, TaskClosed,
ResourceConflictDetected, ResourceReleased, WorkerStateObserved,
WakeRequested, DeliverySucceeded, DeliveryFailed, PluginHealthChanged
```

`control` contains authenticated identity, audience, nonce, expiry,
plugin_instance_id, and route_epoch. Routing, retry, provider, debug, health,
snapshot, wake, and stop semantics remain in that side channel and never enter
`business_payload`.
