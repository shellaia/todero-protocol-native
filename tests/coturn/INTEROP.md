# TURN Interoperability Target (V3)

This project treats `coturn` as the mandatory reference TURN server in CI and local validation.
Behavior must remain aligned with standards-compliant TURN implementations in general.

## Scope
- RFC 5389 STUN basics used by TURN flows.
- RFC 5766/8656 TURN allocation/permission/channel-bind lifecycles.
- Auth challenge and stale-nonce retry handling.

## Mandatory Matrix
- long-term credentials (`username`/`realm`/`nonce`) against coturn
- REST-secret credentials minted by signaling service
- `401 Unauthorized` -> authenticated retry
- `438 Stale Nonce` -> nonce refresh retry
- allocation refresh before expiry
- permission/channel-bind refresh inside negotiated V3 disconnect window
- relay-only policy

## CI Expectation
- CI job must start coturn (`tests/coturn/docker-compose.yml`).
- NAT/TURN integration tests should run against this instance.
- Any behavior divergence from coturn should fail CI until triaged.

## General TURN Compatibility
- The protocol does not depend on coturn-specific packet formats.
- Validation artifacts must focus on RFC-compliant behavior to keep other TURN servers viable.
