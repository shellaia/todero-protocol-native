# NAT/TURN Scenario Matrix

Planned integration scenarios for `v3-nat` + runtime adapters:

1. client behind NAT -> public server (direct + srflx preference)
2. server behind NAT via TURN relay
3. relay/candidate change during active connection
4. forced relay-only path
5. expired TURN creds / permission timeout / allocation expiry

Status: scenarios are defined; automated end-to-end harness implementation is pending.
