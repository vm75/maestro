# Configurable nginx ingress and access policies

Status: Implemented; host acceptance verified. The WAN adapter remains
experimental until the target-device deployment gate below is satisfied.
No deployment or router-support certification is included in this status.

## Verification record

The current implementation has typed profiles/source policies, generated ingress
and deny rules, transactional object rollback, shared certificate windows and
normal offline UI controls. Operator guidance is in [nginx ingress](../../nginx-ingress.md).

Host checks executed during implementation:

- `cargo test` and `cargo test --all-targets`: passed; the final `cargo test`
  includes the expanded-config inventory, complete live ingress checks and
  updated profile lifecycle tests.
  Includes API compatibility, renderer tests, simulated WAN ownership/recovery,
  live nginx isolation, renewal batching, disconnect and shutdown cleanup.
- Focused documented-input, readiness-failure and 60-second batch-deadline tests:
  passed. The deadline test stops a long-running job and prevents the next job
  from obtaining a fresh timeout/window. Synthetic inputs
  load and render; failed challenge readiness removes temporary listeners/tokens
  without starting ACME work.
- `cargo fmt --all -- --check` and
  `cargo clippy --all-targets -- -D warnings`: passed.
- Python nginx suite: 16 tests passed, including 192 base combinations, 24 extra
  combinations, 16 legacy variants and live routing/WebSocket checks.
- `make test-linux`: passed using the repository Python environment and a
  temporary real-nginx lab, with firewall enforcement disabled.
- `node scripts/test-nginx-ui.mjs target/debug/maestro /usr/sbin/chromium`:
  passed normal controls, path blocks, retained drafts/advanced routes, stale
  preview handling, container-only controls, narrow layout, keyboard and offline
  failure behavior. No npm/frontend build chain added.
- `cargo build --release`: passed. No deployment or version bump performed.

The live ingress acceptance test now covers nested policies, mounted subfolders,
generated `/Sessions`, named SPA fallbacks, source-header replacement, TCP
forwarder source collapse/shared budgets, unavailable backends, custom HTTP
listener conflicts, actual challenge token serving and HTTP socket closure.
A failed profile/upstream move restores saved inputs, generated outputs and links.
The matrix below maps requirements to evidence; device-only gates are identified
separately rather than inferred from host tests.

Evidence lives in [ingress integration tests](../../../tests/nginx_ingress_test.rs),
[API integration tests](../../../tests/api_integration_test.rs),
[ingress unit tests](../../../src/nginx_ingress.rs),
[HTTP/WAN unit tests](../../../src/certificate_http.rs),
[mutation rollback tests](../../../src/nginx_mutation.rs),
[nginx template tests](../../../config/nginx/tests/) and the
[offline browser check](../../../scripts/test-nginx-ui.mjs).

The existing bundle's `http2 on` directive requires nginx 1.25.1+, which already
supports `ssl_reject_handshake` (1.19.4+). The implementation therefore uses
handshake rejection on the supported bundle baseline, retains fallback
certificates/HTTP rejection and validates the actual module build with `nginx -t`.
It does not add an older-nginx compatibility promise.

Actual router packet paths, firewall restarts and external IPv4/IPv6 closure
remain target-device deployment gates. The IPv4 adapter is not certified by its
file-backed netfilter simulation. Local listener closure after abrupt process
death remains explicitly deferred to startup/manual recovery; do not claim
unconditional crash-safe closure.

## Goal and scope

Allow one managed nginx process to serve separate public and private application
groups, with administrator-defined listener profiles and reusable source-address
policies. A site must exist only on its selected ingress; nested routes must not
weaken their enclosing access restrictions. Include a way to deny administrative
paths inside otherwise accessible applications.

Routine setup must be possible entirely through the UI without editing TOML,
JSON, or nginx text. Configure reusable listeners/access rules once, then select
them when creating or editing a site. Ease of use is an acceptance requirement,
including accurate exposure summaries and safe handling of inherited settings.

Extend the current Rust renderer, TOML inputs, authenticated API, and standalone
UI. Keep deployment roles, hostnames, addresses, and VPN products outside the
implementation. This plan is self-contained and has no dependency on a personal
migration runbook.

General firewall orchestration, VPN installation/policy management, network
segmentation, application account permissions, additional nginx processes, and
PROXY-protocol/source-identity support are outside this change. Existing rate
policies, backend authentication, certificates, and proxy features remain usable.
The limited exception is temporary WAN HTTP access owned by a certificate
validation session, as specified below; it must not become a general router
port-forward editor.

## Original boundaries and implementation locations

| Current boundary | Relevant code |
|---|---|
| Legacy shared TLS listener/policy; named-profile policy split | [`ssl.conf.jinja`](../../../config/nginx/snippets/ssl.conf.jinja) |
| One allowlist and validated global/site inputs | [`nginx_render.rs`](../../../src/nginx_render.rs) |
| Persistent managed sites and object identities | [`nginx_sites.rs`](../../../src/nginx_sites.rs) |
| Profile-aware HTTP forwarding and challenge-only windows | [`nginx.rs`](../../../src/nginx.rs), `reconcile_http_forwarder` |
| Site/route templates and location inheritance | [`templates/`](../../../config/nginx/templates/), [`proxy-routes.conf.jinja`](../../../config/nginx/snippets/proxy-routes.conf.jinja) |
| Authenticated mutations, validation and rollback | [`api/nginx.rs`](../../../src/api/nginx.rs) |
| Serialized manual/automatic certificate operations and dehydrated invocation | [`state.rs`](../../../src/state.rs), [`api/certificates.rs`](../../../src/api/certificates.rs) |
| Offline settings/site editor | [`web/index.html`](../../../web/index.html) |

Managed inputs live in `nginx.inputs_dir/nginx.toml` and `sites.toml`. Policy
templates are embedded in the binary. Custom nginx objects remain authored text.
Extend these boundaries rather than adding another configuration store/editor.

## Configuration contract

Use two named global collections, `listener_profiles` and `access_policies`.
Names follow the existing validated policy-name convention. The following is
TOML uses synthetic loopback bindings, suitable for adaptation to an
unprivileged integration test. The implementation accepts these tables:

```toml
[listener_profiles.external]
https = [{ address = "127.0.0.1", port = 18443 }]
http = [{ address = "127.0.0.1", port = 18080 }]
http_redirect = true

[listener_profiles.internal]
https = [{ address = "127.0.0.1", port = 19443 }]
http = []
http_redirect = false

[access_policies.proxy_clients]
allow = ["127.0.0.1/32", "::1/128"]
```

Each managed domain/subdomain has one `listener_profile` setting. A profile can
contain multiple structured address/port endpoints, allowing local forwarding
and direct private access to the same site without duplicating its definition.
Require at least one HTTPS endpoint. HTTP is optional and off for newly created
profiles; enabling redirects requires an explicit HTTP endpoint and an
unambiguous HTTPS target port and the shared public HTTP policy's Always-on
mode. Profile HTTP settings must not bypass the renewal-only lifecycle below.
Keep TLS/certificate selection shared as today;
this feature does not introduce per-profile certificate management.

Each site/route can select `access_policy`. Reserve built-ins `public` (no new
source restriction), `deny` (reject), and `legacy` (the current `allowed_ips`).
Administrator-defined policies contain IP/CIDR allow entries followed by a
default deny; an empty list denies all. This is network-source authorization,
not user identity or application authentication.

Illustrative `sites.toml` entry using the existing object identity model:

```toml
[sites."subdomain/example.test/app"]
kind = "subdomain"
domain = "example.test"
name = "app"
server_name = "app.example.test"
upstream = "http://127.0.0.1:19000"
enabled = true
listener_profile = "internal"
access_policy = "proxy_clients"
routes = [
  { path = "/admin", match = "exact", access_policy = "deny" },
  { path = "/admin/", match = "prefix", access_policy = "deny" },
]
```

The deny policy must generate an unconditional rejection, without proxying,
rewrites, or SPA fallback. It is not an arbitrary nginx `return`/command field.
Reject children or incompatible proxy/fallback overrides on an explicit deny
route so the configured result is clear.

Subfolders inherit their containing domain's listener; reject a subfolder or
route listener override. Organizationally grouped subdomains have their own
server blocks and may select different profiles from the domain. Keep the
existing domain enablement dependency clear in the UI.

## HTTP-01 renewal windows

### Feasibility decision

The proposed sequence is feasible: close managed nginx HTTP listeners and owned
router WAN TCP-80 access at startup; open both for a scheduled or manual HTTP-01
renewal batch; close both after every operation in that batch has finished,
including failed operations. The lifecycle is implemented; this does not certify
that a particular router configuration supports automatic forwarding control.
The router adapter and external closure still require target-firmware testing.

HTTP-01 requires public port 80 during validation, but does not require an
always-open listener between validations. Keep the window open until the ACME
client finishes: the authority can fetch tokens repeatedly from multiple
locations. See [Let's Encrypt challenge documentation](https://letsencrypt.org/docs/challenge-types/).

Interpret a manual action here as **Issue/Renew**, or a validation test that
actually contacts the ACME authority. Merely reviewing certificates, checking
expiry, or opening the Certificates page must not enable access. One UI setting,
**Certificate validation only**, selects this lifecycle; users should not need
to coordinate separate nginx and router switches.

Before enabling this mode, identify any existing permanent port-80 forwarding
rule and explicitly migrate or disable it. Removing a temporary rule while a
permanent forward remains active does not establish a closed baseline. Prefer
owned transient runtime rules over repeatedly toggling saved firmware forwarding
settings. The lifecycle, failure handling and crash-recovery limits below are
conditions of this feasibility decision.

### Policy and ownership

Add a shared public HTTP policy with UI choices **Off**, **Certificate validation
only**, and **Always on with redirects**. Default new configurations to
Certificate validation only; retain the current always-on HTTP behavior for
existing configurations until the administrator explicitly changes it. This
shared policy owns availability of managed public HTTP endpoints. Per-profile
HTTP settings select eligible endpoints/redirect sites; they cannot independently
reopen a managed public HTTP socket when the shared policy requires it closed.
Show this relationship in the UI rather than adding competing HTTP toggles.

- **Off:** managed public HTTP listeners and owned WAN access are closed;
  HTTP-01 operations fail with an actionable configuration error. DNS-01 remains
  available and never opens HTTP access.
- **Certificate validation only:** close managed nginx HTTP listeners and WAN
  access on Maestro startup, before starting automatic renewal work. Open
  them temporarily only for actual HTTP-01 issuance/renewal, then close them
  when the batch completes. Normal HTTP-to-HTTPS redirects are unavailable.
- **Always on with redirects:** preserve explicitly configured public HTTP
  service and redirects. Renewal cleanup must not close that intentional access.

Keep challenge endpoint configuration independent of application enablement:
an HTTP certificate must work before a public application vhost exists. Use
structured address/port settings and validate conflicts with profiles/custom
objects. External TCP port 80 is required; a platform adapter may map it to the
configured internal challenge port. HTTP-01 cannot validate a name whose public
DNS points only at an unreachable private/overlay address; direct those users
to DNS-01 rather than temporarily changing their DNS or exposing private apps.

The controller owns only declared managed HTTP endpoints and its own temporary
WAN rules. Do not stop nginx, modify HTTPS listeners, or disable unrelated HTTP
services such as the Maestro API or firmware UI. Inventory custom nginx
listeners and other WAN/NAT rules before enabling renewal-only mode; reject or
report conflicts instead of claiming the port is closed while another listener
or rule still exposes it. Disabling the redirect vhost alone is insufficient:
the existing default-drop server also listens on HTTP.

### Batch lifecycle

1. On startup, establish and verify the closed baseline in renewal-only mode,
   reconcile stale session state, and only then allow HTTP-01 work. If cleanup
   fails, report the actual state and block new HTTP-01 sessions; do not report
   successful closure. Do not interrupt an external ACME client: establish sole
   ownership or reject the configuration when another client owns the endpoint.
2. Identify due certificates before opening access. A scheduled status check,
   an empty renewal batch, and a DNS-only batch must leave port 80 closed.
3. Acquire one certificate-session guard shared by scheduled batches, manual
   issuance and manual renewal. Keep the window open across the batch's HTTP-01
   operations, including multi-name validation; one request cannot close access
   needed by another. Continue reporting individual renewal failures while still
   reaching batch cleanup. Do not open a separate window for every certificate.
4. Generate challenge-only nginx HTTP handling for the required certificate
   names. Serve the shared token directory and reject unrelated paths/hosts;
   do not enable application proxying or normal redirects. Ensure nginx's alias
   and dehydrated's `WELLKNOWN` point to the same validated directory. Run
   `nginx -t`, reload, and verify readiness before allowing WAN access.
5. Enable only the owned WAN TCP-80 access needed for validation. Run dehydrated
   through `CommandRunner` and retain access until validation operations finish,
   not just until the first successful request or individual challenge cleanup.
   Verify configured certificate deployment and nginx reload separately from
   merely obtaining certificate files.
6. On completion, partial setup failure, renewal failure, timeout, cancellation,
   or graceful shutdown, close owned WAN access first, then remove the temporary
   nginx HTTP listeners and validate/reload. Preserve unrelated concurrent nginx
   changes. Cleanup must run even if one of its steps fails; retain and surface
   both the renewal result and cleanup errors.

Reuse the existing serialized certificate-operation path in `src/state.rs` for
manual and automatic work. Refactor batch/session ownership explicitly rather
than recursively acquiring the same lock. Hold the nginx mutation lock only for
short state transitions; do not hold it across ACME network waits or a deployment
hook that may need to reload nginx. All regeneration paths, including settings
saves, startup and certificate-definition reconciliation, must consult the same
active-session state so they cannot reopen idle HTTP or remove an active
challenge route. Define consistent lock ordering and test it.

### WAN adapter and recovery limits

On a router hosting nginx, WAN exposure may use router-local `INPUT` rules, NAT
redirection, or existing forwarding. Implement a narrowly scoped platform
adapter only after verifying the target firmware's actual path; do not assume
that a LAN-host DNAT rule applies. Keep router interfaces, paths and internal
ports configurable with test-mode overrides. Use owned transient rules invoked
as program/argument arrays through `CommandRunner`, without arbitrary API shell
commands or repeated saved NVRAM/GUI forwarding changes that rebuild unrelated
VPN/YazFi rules. Restore the closed state after relevant firewall rebuilds;
never re-enable it solely because a stale marker exists.

Handle IPv6 explicitly: removing an IPv4 forward does not close IPv6 ingress,
and advertised AAAA records must reach the correct challenge handler. Verify
external reachability and closure; a LAN self-test alone does not prove either.
If automatic WAN control is unsupported, show it as unsupported and do not
advertise fully automatic open/close behavior. Externally managed WAN rules
require their own verified lifecycle integration.

Startup cleanup and ordinary error handling do not close access while Router
Hub is dead. Persist only enough session ownership/state for recovery, and
include an independent bounded expiry/cleanup mechanism before claiming
crash-safe closure. A background timer in the same Maestro process is not
independent. On expiry, terminate managed validation work as needed and close
access; a later retry starts a new session. Select a finite batch window that
allows multi-certificate validation and retries, rather than assuming the
existing per-command timeout bounds the whole batch. If the independent
mechanism is deferred, document the limitation explicitly: an abrupt crash can
leave HTTP open until startup recovery or manual cleanup. Do not label that
baseline as guaranteed fail-closed.

## UI workflow and usability contract

Extend the existing nginx page and editors; keep the offline HTML/JavaScript
interface and current Save/validate/reload workflow. Avoid a separate wizard,
frontend framework, or additional confirmation step for ordinary saves.

### Configure reusable settings once

Use **Listeners** and **Access rules** as UI labels. The stored fields remain
`listener_profiles` and `access_policies`; users do not need those schema names
to complete setup.

| Editor | Normal controls | Details shown on demand |
|---|---|---|
| Listener | Name; HTTPS address/port rows; Add/remove address; HTTP eligibility under the shared policy | HTTP address/port rows when enabled; IPv6 help; binding conflicts |
| Access rule | Name; allowed IPs/networks, one per line | CIDR examples; effective restrictions; source-address limitations |

Show configured entries as compact rows/cards with their endpoints or allowed
sources, number of referencing sites, and Edit/Delete actions. Deletion errors
identify referencing sites/defaults/routes, including disabled sites. Show which
sites a shared-rule edit affects before the existing Save action; do not make
users discover the effect by opening each site.

New listener forms start with loopback binding and HTTP redirects off. Adding
LAN/wildcard bindings requires an explicit address choice; do not infer trusted
subnets or expose a service on every interface automatically. Label wildcard
bindings as **All IPv4 interfaces** / **All IPv6 interfaces**. An address describes
where nginx listens, not whether the firewall/VPN permits access.

Keep the existing legacy listener and allowlist editable through these same
controls, with an **Existing configuration** label. Do not show competing old
and new listener/allowlist editors whose relationship users must work out.
Preserve the underlying compatibility fields as specified below.

Expose the shared HTTP policy and supported WAN adapter settings using normal
form controls. In the Certificates view, show **HTTP closed**, **Validation in
progress**, or **Cleanup failed / exposure unknown**, together with the active
batch and window deadline where supported. Clearly distinguish configured mode
from observed runtime state. Issue/Renew buttons use the same lifecycle as
scheduled work; users must not manually toggle forwarding before each renewal.
Explain that validation-only mode removes ordinary HTTP redirects and that
DNS-01 never needs this window. Preserve existing settings until explicitly
migrated; do not silently switch public sites out of always-on mode.

### Create or edit a site

Keep the normal form focused on identity, upstream, **Listener**, and
**Access rule**, with optional **Blocked paths**. Existing proxy tuning and
advanced routes remain available under expandable sections. Do not redesign
unrelated settings as part of this work.

- Listener choices display the configured name and HTTPS endpoints. Subfolders
  show the containing domain's listener read-only; subdomains select their own.
- Access choices include named rules and explicit **Use inherited rule**,
  **Any source**, and **Block all** choices. Map the last two to `public` and
  `deny`; do not label `public` as a guarantee of internet reachability. Show the
  resolved inherited name instead of a blank selection or raw boolean.
- Preserve existing effective selections when editing. New sites use the
  configured defaults and remain disabled initially; choosing a listener never
  automatically changes the access rule to Any source or enables the site.
- The **Blocked paths** control accepts paths with an **Include subpaths**
  checkbox. Blocking `/admin` with subpaths produces the exact `/admin` and
  prefix `/admin/` deny routes from the schema example, without blocking
  `/administrator`. Users do not need to author both route objects manually.
  This is a generic convenience over the same route model, not application
  detection or a second policy store.
- Link to listener/access-rule management while preserving the unsaved site
  draft and refreshing choices on return. Avoid requiring users to re-enter the
  site to create a missing reusable setting.

Show a compact, live **Effective access** summary alongside these controls:

- Selected listener name, actual bindings, and whether HTTP redirects exist.
- Selected/inherited source rule and any enclosing restriction that further
  limits it. If policies intersect to an empty set, state **No matching sources;
  requests will be blocked**. Any source on a child must not appear to override
  a restricted parent.
- Blocked paths and an indication when a site's containing domain is disabled.

Derive effective configuration through the same backend resolver used for
validation/rendering; expose the necessary structured results for editing and
list responses. Do not implement a second policy resolver in JavaScript. Resolve
draft changes without applying nginx configuration; discard stale responses if
users change selections quickly. Keep errors next to the relevant fields and
retain drafts after validation/reload failures.

The sites table shows listener and access-rule names with a short effective
summary so exposure can be reviewed without opening every site. Custom objects
are marked **Custom configuration** and must not receive a managed-isolation
badge inferred from a label or incomplete parsing of authored nginx text.

### Keep advanced configuration available

Provide a structured editor for ordinary path/access rules, including inherited,
named, and deny selections. Keep existing regex, nested route, rewrite and SPA
settings in an advanced section; JSON may remain an optional advanced view, but
must not be required for listener assignment, allowlists or path blocking.
Editing the simple controls must preserve unrelated advanced route properties
and ordering. If a route cannot be represented by a simple row, show it as
advanced instead of silently rewriting or dropping it. Detect conflicting
blocked-path and advanced rules before save.

Place explanations where they affect a decision: for example, explain beside
loopback source rules that a local forwarder can hide individual client IPs.
State that VPN grants, general firewall permissions and application accounts are
managed outside this form; the scoped HTTP-01 WAN lifecycle is the explicit
exception. Do not describe a private-looking profile name as proof that those
external controls are configured.

## Security invariants and rendering

1. **Explicit exposure.** Render only the selected profile's endpoints into a
   domain/subdomain. Adding a profile must not attach existing sites to it.
   Move listener directives out of the shared TLS-policy snippet. Preserve its
   established compatibility role for authored objects using the old include.
2. **Rejecting defaults.** Generate one default rejection server per unique
   managed address/port endpoint. Reject unknown HTTP hosts as well as unknown
   TLS SNI. Use `ssl_reject_handshake` only where supported by the installed
   nginx; otherwise retain a valid fallback certificate plus HTTP rejection.
   A default server alone must not be mistaken for a strict Host allowlist when
   a known-SNI connection sends a different Host; test and enforce that case.
3. **Monotonic restrictions.** Compute effective source access as the intersection
   of enclosing domain, mounted application, and route policies. `public` on a
   child adds no restriction; it cannot remove a parent restriction. A deny
   ancestor stays denied. Materialize the effective allowlist (or equivalent
   cumulative checks) rather than relying on nginx `allow`/`deny` inheritance,
   which is replaced by child rules. Compute IPv4 and IPv6 intersections safely;
   preserve deny-all results instead of rendering an empty unrestricted block.
4. **Complete location coverage.** Apply effective policy to proxy roots, nested
   and regex routes, mounted subfolders, generated `/Sessions`, and named SPA
   fallbacks. Validate rewrite/internal-redirect behavior so it cannot enter a
   less restricted backend path. Denied routes must never reach an upstream.
5. **Profile-aware HTTP.** Private profiles with HTTP disabled contribute no
   redirect hostnames to public ingress. Keep HTTP-01 certificate-only challenge
   hosts available according to the shared HTTP policy and active renewal
   window, without enabling private application redirects.
   Redirects for explicitly enabled profiles must use the correct HTTPS port.
   Keep forwarded authority/port and application redirects correct for
   nonstandard TLS ports without trusting unvalidated client headers.
6. **No implicit source trust.** Preserve replacement of untrusted forwarding
   chains. A local TCP forwarder can collapse client addresses; profile/policy
   names do not recover that identity. Document shared rate budgets and require
   operators to verify ban exceptions for shared forwarding addresses. Do not
   disable authentication or all private-ingress abuse controls automatically.
7. **Safe mutation.** Profile/policy edits, deletes, site moves and regeneration
   use the existing nginx lock, protected paths, atomic file writes, validation,
   reload, and rollback. Include stale generated include removal in rollback;
   removing an unused endpoint must not leave an old listening default server.

## Validation and compatibility

- Parse structured literal IPv4/IPv6 addresses and integer ports; bracket IPv6
  correctly in nginx output. Do not accept arbitrary `listen` directives or
  interpolated nginx text. Bound collection sizes and generated output using
  limits covered by validation tests.
- Validate references across all managed inputs, including disabled sites,
  nested routes and defaults. Reject deletion/rename while referenced. Reject
  site-only fields in routes and listener overrides in subfolders/default route
  objects.
- Reject duplicate or overlapping endpoint ownership across profiles, including
  wildcard-versus-specific bindings of the same address family. Within a
  profile reject duplicate endpoints and HTTP/TLS socket collisions. Do not
  infer that different profile names establish separate socket boundaries.
- Reject ambiguous hostname ownership on an endpoint. Existing upstream maps
  are global by hostname: also reject different managed upstreams for the same
  hostname across profiles unless map scoping is explicitly implemented. Keep
  this first change small by rejecting ambiguity rather than redesigning maps.
- Preserve old `router_ip`, `http_port`, `https_port`, `allowed_ips`,
  `enable_lanonly`, and `enable_restricted_sessions` inputs. Synthesize a reserved
  legacy listener profile from old globals. Sites without a new assignment keep
  their existing listener and effective access behavior; creating another
  profile must not migrate them.
- Treat omitted access policy as the old effective `enable_lanonly` behavior:
  true/default selects `legacy`, false selects `public`, still constrained by
  ancestors. Reject an explicit new policy together with an explicit
  `enable_lanonly` at the same scope rather than silently choosing precedence.
  Resolve global defaults before site/route overrides and show effective values.
  Give generated restricted-session routes an explicit documented policy and
  support a named `restricted_sessions_policy` override, defaulting to legacy.
- Do not rewrite authored custom objects or promise isolation for unmanaged
  configurations. Inventory their listener/default-server/includes before a
  profile migration. Shared compatibility includes retain legacy semantics;
  endpoint conflicts must stop nginx validation and restore prior files.
- Keep existing API payloads and offline UI behavior compatible. Retain legacy
  fields in read/write round trips until an explicit migration replaces them;
  old clients must not accidentally erase new referenced profiles/policies.
  Reject unsafe replacement requests with an actionable error.

## Implementation sequence

### 1. Inputs and effective configuration

- [x] Add typed listener/access-policy data and validated policy references in
  `src/nginx_render.rs`; persist them in the current TOML/API schema.
- [x] Resolve legacy inputs, defaults, site membership, parent restrictions and
  restricted sessions into an effective configuration before rendering.
- [x] Validate the combined managed tree and global upstream-map constraints.
  Reuse `src/nginx_sites.rs` object identities and existing storage protections.
- [x] Add focused schema, legacy round-trip, reference and intersection tests.

### 2. nginx generation and transactional application

- [x] Separate managed listener output from TLS policy; generate rejecting
  defaults and access-policy output with embedded templates.
- [x] Add deny routes and propagate effective access through mounts, nested
  locations and SPA fallbacks.
- [x] Update HTTP forwarder reconciliation, including startup and certificate
  workflows, to preserve challenge-only hosts and profile-specific redirects.
- [x] Extend object/global mutation transactions to include generated-file
  creation and removal. Verify rollback on test/reload/command failures.
- [x] Preserve authored objects and legacy shared include behavior.

### 3. Certificate HTTP lifecycle

- [x] Add the shared HTTP policy with legacy-compatible defaults and a closed
  startup baseline for renewal-only mode; include every managed HTTP listener.
- [x] Refactor scheduled batches and manual issue/renew actions around the same
  serialized session lifecycle, opening only for actual HTTP-01 work.
- [x] Generate challenge-only routes from certificate names and use one token
  directory across nginx and dehydrated; preserve HTTPS/app isolation.
- [x] Add scoped platform WAN-access control and test-mode simulation, including
  rule ownership, IPv6 blocking, conflicts and firewall-rebuild recovery.
- [ ] Deployment gate: verify router-local/NAT behavior, IPv6, conflicts and
  firewall restart handling on the actual target before declaring the adapter
  supported. Host implementation and simulations do not satisfy this gate.
- [x] Add unconditional cleanup, startup reconciliation, batch-window limits and
  independent crash cleanup, or explicitly document the latter as deferred
  without claiming crash-safe closure. Surface cleanup failures separately.
- [x] Test lock ordering, concurrent regeneration and certificate deployment;
  never rely solely on dehydrated hooks for opening/closing the window.

### 4. API and offline UI

- [x] Extend current authenticated nginx settings endpoints; use the existing
  nginx mutation lock for all affected operations.
- [x] Expose backend-resolved effective configuration and draft validation for
  summaries without applying changes; keep one authoritative policy resolver.
- [x] Add profile and policy management to global settings; expose address/port
  fields and IP/CIDR lists rather than arbitrary nginx source or required JSON.
  Include reference counts, affected-site details and unified legacy editing.
- [x] Add listener selection for domain/subdomain sites and access selection
  for sites/routes, including deny. Show inherited/effective restrictions,
  actual endpoints and HTTP exposure; explain that labels do not identify users.
- [x] Add simple blocked-path controls and structured path/access rows over the
  existing route model. Preserve advanced properties, route ordering and drafts.
- [x] Keep the normal site form concise, show listener/access summaries in the
  sites table, and disclose existing tuning/advanced settings on demand.
- [x] Explain why a referenced profile/policy cannot be deleted and preserve
  unrelated settings on edits. Keep the self-contained UI offline-capable.
- [x] Add shared HTTP mode/WAN capability controls and certificate-window status
  without raw JSON or manual forwarding toggles for each renewal.

### 5. Verification and operator documentation

- [x] Add Rust renderer/API tests and real-nginx integration coverage in the
  existing `tests/` and `config/nginx/tests/` suites.
- [x] Update `config/nginx/README.md`, synthetic examples, `docs/configuration.md`,
  `docs/migration.md`, and `docs/architecture.md` when implementation makes their
  existing descriptions inaccurate. Update release history per release policy.
- [x] Document legacy migration, authored-object conflicts, source-address loss,
  application admin-path restrictions and nonstandard-port client behavior.
- [x] Complete the acceptance matrix and repository checks below before release.

## Acceptance matrix

Use temporary directories, loopback endpoints and mock backends. A positive
request must prove backend reachability; a denied request must prove the backend
received nothing. Test enabled and disabled managed objects and custom objects.

| Scenario | Required evidence | Implementation evidence / deployment gate |
|---|---|---|
| Existing inputs without new fields | Same effective endpoints, allowlists, routes and HTTP behavior | API legacy round trips; native renderer and Python legacy variants |
| New profile added without site reassignment | No existing site's exposure changes | Documented-input test compares legacy output with/without an unused profile; assignment selects the profile |
| Site assigned to an internal profile | Reachable only on that profile's endpoints; absent from external HTTP redirects | Live ingress: positive internal response; public TLS and HTTP requests rejected |
| Separate administrative site | Internal application ingress cannot serve it with crafted Host/SNI | Live ingress: management hostname rejected on application endpoint |
| Unknown SNI, unknown Host, known SNI with unknown/different Host | No unintended backend, including persistent connections | Live ingress: wrong endpoint/SNI, unknown Host and known-SNI/different-Host requests rejected |
| Denied administrative path | Exact path, descendants, query strings and normalized/encoded variants never proxy | Live ingress: exact, descendant, query and encoded/normalized requests; backend counter unchanged |
| Parent restriction plus public/different child policy | Effective intersection for roots, routes, mounts, `/Sessions` and SPA fallbacks | Live ingress: domain/mount/nested route, generated Sessions and SPA access; intersection unit tests |
| Empty/disjoint source policies | Deny all, including IPv6 | IPv4/IPv6 intersection and empty-policy renderer unit tests |
| HTTP off / explicit nonstandard-port redirect | No private redirect / correct configured target | Disabled HTTP lifecycle test; live redirect target and trusted forwarded-port checks |
| HTTP-01 certificate without a public application | Challenge works; private application does not become public | Live challenge-only hostname serves a token; other paths/hosts cannot proxy |
| Renewal-only startup, status checks, no due certificates, DNS-only batch | Managed public HTTP and owned WAN access remain closed | Startup/session and DNS/off tests; real idle/cleanup socket closure; browser certificate-status check |
| Scheduled batch or manual issue/renew with HTTP-01 | nginx challenge handler ready before WAN opens; one window spans the batch; unrelated HTTP paths rejected | Batch/disconnect tests; readiness-failure test; live token and socket transitions |
| Multi-name/multi-certificate batch with one failure | Remaining results reported; access closes after batch completion regardless of partial failure | Automatic batch test includes a SAN, continues after failure and closes once |
| Setup failure, command timeout, cancellation or graceful shutdown | WAN closure attempted first; nginx cleanup still attempted; actual closure/error state reported | Readiness failure, 60-second deadline, disconnect, shutdown and WAN cleanup-failure tests |
| Abrupt process death and restart | Stale access removed before new work; independent expiry verified if crash-safe closure is claimed | Stale-session/ownership recovery tests and startup ordering; independent local cleanup explicitly deferred, no crash-safe claim |
| Batch exceeding its maximum window | Work stopped safely and access closed; per-command timeout does not reset the batch deadline | 60-second end-to-end deadline test: first command killed, next command not started, HTTP closed |
| Concurrent manual renewal, settings save or certificate-route reconciliation | No premature closure/reopening, lost settings or lock deadlock | Owned operation guard plus batch test: concurrent settings and certificate-route regeneration preserve the active window |
| IPv4/IPv6 and target firewall restart | Correct challenge reachability only during the window; closed baseline restored without unrelated rule changes | WAN command simulation verifies ordering, IPv6 block and rebuild closure; actual packet paths remain a deployment gate |
| Always-on mode and legacy configuration | Intentional redirects/listeners survive renewal cleanup; migration to renewal-only is explicit | Legacy API and native/template tests; Always-on ownership recovery; explicit UI mode migration |
| UI: HTTP mode, WAN capability and window status | Normal controls; active/closed/failed states reflect lifecycle results rather than only configured intent | Browser mode/WAN saves and closed status; backend active/cleanup-failure/retry tests; adapter labeled as requiring firmware verification |
| Multiple sites behind a local TCP forwarder | Source attribution/shared rate budgets documented and demonstrated; no header-based identity spoofing | Live TCP forwarders demonstrate source collapse and cross-site HTTP 429 shared budget; forged headers cannot bypass source checks |
| Invalid reference, socket collision or ambiguous hostname/upstream | Request rejected without active configuration changes | Schema, reference, endpoint-overlap, collection-limit and case-insensitive hostname-conflict tests |
| Site move, profile removal, nginx test/reload failure | Old files, enabled links, inputs and service configuration restored; no stale listener includes | Failed profile/upstream move test; global validation/reload rollback tests; documented-input test verifies referenced-profile rejection and removal of the unused rejecting listener |
| Media backend offline | nginx starts/reloads; independent application remains reachable | Live nginx starts with offline backend, returns 502 there and 200 for independent application |
| Legacy API/UI round trip and authored objects | New referenced settings retained or unsafe replacement rejected; custom content preserved | API compatibility, custom-file rollback/mode/link preservation and browser advanced-route round trip |
| Restart/startup and certificate reconciliation | Same isolation/redirect result as API mutation path | Startup order and stale-session tests; active-batch certificate-definition regeneration test |
| UI: create listener/rule, assign a site, block a path with descendants | Complete using normal form controls, with no TOML/JSON/nginx editing | Browser ordinary controls persist profile/rule/site and exact/descendant denies |
| UI: edit inherited access or a shared rule | Effective restriction and affected sites visible; no silent broadening | Backend preview/reference resolver and read-only API tests; browser current/stale effective preview checks |
| UI: simple/advanced route edits and failed save | Route properties/order and unsaved draft preserved; conflict/error shown in context | Browser preserves nested/transport route data, unsaved drafts and offline error context |
| UI: desktop, narrow/mobile layout, keyboard-only interaction, network offline | Controls, summaries and validation usable; no CDN/build dependency | Browser layout/keyboard/offline checks, mobile screenshot inspection and no external asset requests |

Exercise the UI tasks against the existing test-mode API and verify the actual
saved inputs/rendered behavior, not only the displayed labels. Check that draft
summaries never mutate configuration and stale responses cannot mislabel the
current selection. A documented API-only workaround does not satisfy the normal
UI tasks above. Use existing tooling or focused manual checks; do not add a
frontend build/test framework solely for this feature.

Run the existing commands, with installed prerequisites as documented:

```sh
cargo fmt --all -- --check
cargo test
cargo clippy --all-targets -- -D warnings
make test-nginx
make test-linux
cargo build --release
```

These checks establish host-side behavior, not router/firmware compatibility.
Before deployment, verify the actual nginx build, bindings, certificates, firewall
and startup behavior on the target router. Keep matching-device validation and
personal deployment acceptance outside repository claims.
