# Upgrading older Router Hub installations

**Moving from Router Hub 0.10.5 to Maestro 0.11.0?** Follow the [Maestro rebrand migration](maestro-migration.md) first. This page documents older Router Hub schema migrations, which remain relevant to legacy inputs.

Current releases read versioned ban state and current nginx settings only. They do
not convert older document schemas. Back up the installed binary, Router Hub configuration
and state directory, and the nginx configuration tree before upgrading. Use the
paths configured on your installation; router and native Linux paths can differ.

## Nginx input relocation in 0.9

Version 0.9 moves current-format nginx inputs from
`<nginx.root_dir>/settings/` to `<nginx.inputs_dir>/`, which defaults to
`/opt/etc/router-hub/nginx`. The router installer stops Router Hub and runs the
relocation command. For a manual binary upgrade:

1. Back up Router Hub's configuration, nginx's tree, and the old binary, then
   stop Router Hub. nginx may keep serving its existing runtime configuration.
2. Install the new binary and set `[nginx].inputs_dir` if using a non-default
   layout. Keep it separate from the legacy settings directory. For Linux,
   select a writable path alongside the service configuration.
3. Run the new binary with the same service configuration:

   ```sh
   router-hub --config /opt/etc/router-hub/router-hub.toml migrate-nginx-inputs
   ```

4. Review the relocated `nginx.yaml`, `sites.yaml`, and `.conf`/`.txt` data
   fragments. Start Router Hub and save Global settings to regenerate and validate
   the combined runtime tree before reloading a running nginx.

The command handles only known current-format inputs. It preserves file contents,
rejects differing destination files, validates settings and named-policy references,
and rolls back a failed migration. Resolve conflicts explicitly using your backup;
there is no force-overwrite option. Re-running a completed migration is safe.
Legacy `.toml` and `.txt` inputs are removed after successful validation, while
legacy `.conf` files stay in place as active nginx includes. Regeneration updates
those includes from the new input directory. Custom nginx objects, certificates,
unknown files, and Jinja files are untouched. Jinja policies remain embedded;
on-disk template copies are not used by Router Hub.

Startup and input reads reject unmigrated legacy TOML files when the new inputs
are missing, rather than treating the installation as empty. Back up the new
input directory and custom nginx files thereafter. To downgrade, restore the
matching legacy inputs and binary from the pre-upgrade backup.

## Named ingress and renewal-only HTTP

Sites without a listener assignment retain the synthesized `legacy` listener.
Old `enable_lanonly`/allowlist settings remain supported; do not combine an
explicit `access_policy` with `enable_lanonly` in the same settings object.
Existing global inputs without `http_access` keep always-on HTTP behavior.
Changing to validation-only is explicit and removes ordinary HTTP redirects.
Back up inputs and custom objects, inventory custom listeners and permanent
forwarding rules, then configure and verify the desired WAN lifecycle before
turning on renewal-only mode. New inputs default to renewal-only with no
automatic WAN adapter configured. See [ingress operations](nginx-ingress.md)
for source-address caveats, firmware verification and crash-recovery limits.

## Versions older than 0.7.2

First install and run 0.7.2 using the same configuration and state directory.
Installing the binary alone does not migrate bans: the firewall policy must be
enabled and have configured log files so the ban engine starts.

Check the logs for migration or persistence failures. Confirm that
`ban-attack-state.json` in the configured data directory contains
`"schema_version": 1`. When importing `bans.json`, 0.7.2 normally archives it as
`bans.json.migrated-v1` after writing the new document. If versioned state already
exists, it takes precedence over `bans.json`; resolve any unexpected stale state
before continuing. Do not manufacture an empty state file to bypass migration.

0.7.2 imports unexpired records from the old ban-record array. Records without
expiration and the older IP/subnet-list format receive a one-hour lifetime.
Expired bans are not preserved. Inspect active bans in the UI before proceeding.
Stop 0.7.2 cleanly and retain its versioned state when installing the new release.
If the firewall is intentionally disabled, either complete migration before
disabling it again or explicitly accept discarding the old transient bans.

## Nginx configuration

The 0.7.2 bridge handles ban state; it does not port old nginx configuration to
the current Jinja bundle. Prepare and validate nginx separately before switching
Router Hub versions.

- Remove the obsolete `nginx.templates_dir` option from `router-hub.toml`.
- Use `<nginx.inputs_dir>/nginx.yaml` for shared settings under `[global]`, site defaults
  under `[site_defaults]`, and top-level `allowed_ips` and `free_subdomains` lists.
  Copy any entries from `allowed_ips.txt` and `free_subdomains.txt` into these
  lists; strip comments. The fragments are generated outputs after saving.
  Copy any separately stored global JSON settings manually into the YAML schema.
- Put managed site inputs in `<nginx.inputs_dir>/sites.yaml` using the
  [current examples](../config/nginx/examples/sites.yaml). Remove `output` fields;
  output paths are derived from site identity. Preserve upstreams, hostnames,
  enablement and per-site settings. Copy settings from generated `router-hub-settings` comments when needed,
  but review and save them in YAML; saving shared settings no longer adopts
  generated files automatically or treats their comments as managed inputs.
- Replace `enable_no_cache` and `enable_pwa` with `cache_policy` in site defaults,
  sites, routes and nested children. An existing explicit `cache_policy` wins;
  otherwise true `enable_no_cache` becomes `"no_cache"`, then true `enable_pwa`
  becomes `"pwa"`. If both are false, omit the policy to preserve inheritance
  (the site default is `"inherit"`). Remove both old keys after conversion.
  Account for effective site defaults when converting site overrides.
- Preserve custom authored server configuration. Port its old include paths to
  the [current runtime files](../config/nginx/README.md), and review any custom
  maps or routing rules against the new bundle. The old template/file editors
  and automatic nginx conversions are unavailable.

Prepare the replacement tree in a lab or staging location with suitable paths.
Validate with `router-hub --config <config-path> check-config` and nginx's `-t`
using the intended configuration. Check HTTP redirects, ACME challenge routing,
TLS, upstreams, access restrictions, cache headers, and custom routes before
activating it. `init-nginx` takes over the main configuration and saves a
different existing file as `nginx.conf.before-router-hub-<unique-id>`. It preserves
existing shared/custom files and saved inputs; it does not convert their contents.

## After upgrading

Confirm that firewall policy, allowlists, active unexpired bans, certificates,
and nginx sites have the expected behavior. Keep the backup until verification
is complete. To roll back, stop the new service and restore the matching old
binary, configuration and state backup together.

## One-time TOML to YAML migration

Maestro now reads and writes YAML configuration on both ASUS and Linux. Existing
TOML configuration is conversion input only; it is never a runtime fallback.
Stop the service before migrating. For an existing Maestro installation:

```sh
/opt/etc/init.d/S99maestro stop
/opt/bin/maestro --config /opt/etc/maestro/maestro.toml migrate-config
/opt/bin/maestro --config /opt/etc/maestro/maestro.yaml check-config
/opt/etc/init.d/S99maestro start
```

For Linux, run `migrate-config` as the workload account with its actual legacy
configuration path. Update any custom service command or `--config` argument to
the resulting `.yaml` path. The router installer performs this conversion once
while its service is stopped. Existing native Linux YAML needs no conversion.

The command preserves configured values, relative path meanings and credentials.
It converts the main configuration and managed `nginx.toml`/`sites.toml` inputs,
and relocates old nginx input files through the existing migration workflow.
Exact original bytes, including comments, remain in private `.toml.bak` files;
converted YAML is normalized and does not copy TOML comments. State, journals,
certificates and authored native nginx configuration are not format-converted.

Migration refuses symlinks, conflicting destinations, changed source files and
invalid TOML. It never overwrites a different YAML file or backup. After success,
the active TOML source is removed. Repeating the command against its old path is
safe and preserves later YAML edits. If interrupted, rerun it; matching completed
files are retained and conflicting files require inspection. Keep backups private:
they contain the original credentials. Do not run an older binary against the
same installation until its supported configuration has been restored while the
service is stopped.

For new installations, use `config/maestro.example.yaml` on ASUS or the non-root
`init-linux-config` command on Linux. JSON persistence files and native service
configuration retain their existing formats. Cargo's TOML build manifests are
unrelated to Maestro's operator configuration.
