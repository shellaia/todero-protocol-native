# Coturn Harness

Bring up a local coturn instance for TURN compatibility checks:

```bash
cd protocol-v3
./scripts/coturn-up.sh
# run TURN integration checks
./scripts/coturn-down.sh
```

Defaults:
- realm: `todero.local`
- long-term user/pass: `testuser` / `testpass`
- static auth secret: `turn-shared-secret`
- TURN port: `3478`
