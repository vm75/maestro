# One-time Router Hub → Maestro migration (v0.11.0)

**Maestro v0.11.0 is a clean break, not a runtime compatibility release.**
The source is now at [vm75/maestro](https://github.com/vm75/maestro).
Old CLI aliases, browser tokens, static-asset routes, service aliases, NGINX
settings markers and firewall ownership are not recognized by Maestro.
`Conflicts: router-hub` remains in the IPK only to prevent co-installation.

**This procedure has not been tested on physical ASUS hardware.** It is for
the sole existing Router Hub installation. Do not start without local SSH or
console access, a working rollback package, and verified off-router backups.
A `refactor/rebrand-maestro` branch is not itself a published v0.11.0 IPK.

## 1. Preserve a complete, independent rollback

While Router Hub is still working:

1. Run `/opt/bin/router-hub backup` and copy the resulting archive **off
   the router** immediately. `/tmp` is volatile.
2. Separately back up `/opt/etc/router-hub`, `/opt/var/lib/router-hub`,
   `/opt/etc/nginx`, certificates/private keys, `/jffs/scripts`,
   `/opt/etc/init.d`, and **custom external paths** in the old TOML.
3. Save the old v0.10.5 IPK, and capture `iptables-save`,
   `ip6tables-save`, and `ipset save` for reference.
4. Verify the backups can be read and restored. A backup left only on the
   router does not satisfy the gate.

## 2. Stop Router Hub and stage Maestro explicitly

Copy or check out this repository's
`scripts/migrate-router-hub-once.sh` on the router, review it, then run:

```sh
/opt/etc/init.d/S99router-hub stop
MAESTRO_BACKUP_VERIFIED=YES sh scripts/migrate-router-hub-once.sh
```

**This script is operator-invoked, never bundled or called by the IPK.**
It requires backup confirmation, refuses to overwrite existing
`/opt/etc/maestro` or `/opt/var/lib/maestro`, and copies rather than
moves the two old trees. It renames the copied TOML to `maestro.toml`,
converts known absolute paths and default ipset set names in copied
TOML/JSON, and converts `# router-hub-settings:` markers into
`# maestro-settings:` markers in generated `.conf` files in the copied
trees and the **standard** `/opt/etc/nginx` tree. That shared tree is
modified: the independent NGINX backup is essential for rollback.

Review before continuing:

```sh
ls -l /opt/etc/maestro/maestro.toml
grep -nE 'router[-_]hub|ROUTER_HUB' /opt/etc/maestro/maestro.toml || true
grep -nE 'data_dir|inputs_dir|certs_dir|dns01_hook|dehydrated|set_name_' /opt/etc/maestro/maestro.toml
```

Confirm the token, NGINX sites, certs, dehydrated executable/hooks, firewall
policy, ban state, service settings and external custom paths resolve to
existing intended files. If NGINX uses a **custom root**, manually convert
embedded settings markers in that tree after backing it up. Do not rewrite
arbitrary domain names, hostnames or unrelated service configuration. The
helper cannot infer all custom paths.

## 3. Retire legacy startup hooks and firewall state

Inspect `/jffs/scripts/post-mount` and `/jffs/scripts/firewall-start`.
**Manually remove entire Router Hub-owned reconciliation blocks**, leaving
your other commands untouched:

```sh
grep -nE 'router-hub|Router Hub|S99router-hub' /jffs/scripts/post-mount /jffs/scripts/firewall-start || true
```

If the old init script was installed manually (not owned by an IPK),
disable it after backing it up. Once the data is staged and inspected,
uninstall the old package:

```sh
/opt/bin/opkg remove router-hub
```

**Reboot before installing Maestro** (with local admin access). Router
firewall chains/ipsets normally live in memory; reboot avoids dangerous
manual flushing and confirms the retired JFFS hooks cannot recreate them.
After reboot, verify these old chains/sets **do not exist**:

```sh
/opt/sbin/iptables -nL ROUTER_HUB_INPUT 2>&1 || true
/opt/sbin/iptables -nL ROUTER_HUB_FORWARD 2>&1 || true
/opt/sbin/ip6tables -nL ROUTER_HUB_INPUT 2>&1 || true
/opt/sbin/ip6tables -nL ROUTER_HUB_FORWARD 2>&1 || true
/opt/sbin/ipset list router_hub_bans4 2>&1 || true
/opt/sbin/ipset list router_hub_bans6 2>&1 || true
```

If any legacy object survives reboot, diagnose the external boot hook or
owner. **Do not blindly flush or destroy shared firewall objects.**
Do not run Router Hub and Maestro concurrently.

## 4. Install the released Maestro package

Only after `v0.11.0` is published:

```sh
VERSION=0.11.0
FILE="maestro_${VERSION}-1_aarch64-3.10.ipk"
BASE="https://github.com/vm75/maestro/releases/download/v${VERSION}"
cd /tmp
curl -fLO "${BASE}/${FILE}"
curl -fLO "${BASE}/${FILE}.sha256"
sha256sum -c "${FILE}.sha256"
/opt/bin/opkg install "/tmp/${FILE}"
```

The normal post-install helper uses only Maestro data. Its
`migrate-nginx-inputs` operation remains for NGINX input-layout migration
**within Maestro**, not for Router Hub data import. It installs fresh
Maestro JFFS hooks and checks configuration.

```sh
/opt/bin/maestro --config /opt/etc/maestro/maestro.toml check-config
/opt/etc/init.d/S99maestro check
/opt/bin/maestro backup
```

Verify ASUS menu and authentication, site CRUD/reload, HTTPS certificates,
AdGuard/DNS, WireGuard, WOL, Services, Ban Shield, backups, logs, and reboot
persistence. With Ban Shield enabled, check `MAESTRO_INPUT`,
`MAESTRO_FORWARD` and `maestro_bans4`/`maestro_bans6`. Compare against
the baseline. **Do not delete the original Router Hub directories** merely
because the Maestro service starts.

## Rollback

Stop/remove Maestro, restore NGINX and JFFS from the **pre-migration** backup
(the marker conversion changed generated nginx files), reinstall the verified
Router Hub v0.10.5 IPK, and reuse the untouched Router Hub configuration and
state directories. Restore the original startup hooks and verify old
firewall/NGINX behavior after reboot. An IPK reinstall alone will **not**
undo changes to external NGINX inputs or JFFS hooks.

## Acceptance gate

Run formatting, Rust tests, Clippy, release build, UI checks, IPK and native
Linux tests, mocked QEMU tests and
`bash tests/test_one_time_migration.sh`. Also validate the real ASUS
router's clean install, one-time migration, firewall recovery, reboot and
rollback before Phase 2. The staging helper is the **only** new code that
intentionally knows Router Hub persistence names; runtime Maestro does not
provide old-version compatibility.
