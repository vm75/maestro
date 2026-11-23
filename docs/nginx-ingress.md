# Nginx listeners, access rules and certificate windows

Configure reusable **Listeners** and **Access rules** in Nginx → Global settings.
Select them in each managed site's editor. The effective-access preview resolves
inherited settings on the server without saving them. The sites table shows the
selected listener and bindings. These controls need no frontend build or network
assets.

## Harden an existing legacy installation

Keep a working LAN/VPN management session open and back up your nginx inputs and configuration before changing access.

1. Keep management private

   Keep the ASUS administration UI, Maestro, SSH, and application administration interfaces reachable only through your trusted LAN or VPN. Check firmware port forwards and WAN-management settings too: nginx rules only protect traffic passing through nginx.

2. Create named access rules

   In Nginx → Global settings → Access rules, create rules such as:

   | Rule            | Allowed sources                                           |
   | --------------- | --------------------------------------------------------- |
   | admins          | Your administrator devices or VPN administrator addresses |
   | trusted         | Your actual trusted LAN and VPN subnets                   |
   | Built-in public | Anyone                                                    |
   | Built-in deny   | Nobody                                                    |

   Use your actual networks, including IPv6 where applicable. Avoid allowing entire private address ranges unnecessarily.

   Edit one private site and select trusted or admins. Replace its legacy LAN only setting; explicit access_policy and enable_lanonly cannot coexist in the same settings object.

   Check the effective-access preview, save, and test before migrating the next site. A child rule cannot widen its parent’s restrictions.

3. Separate private and public listeners where useful

   Under Global settings → Listeners, add a private HTTPS listener bound to an address assigned to the router’s LAN/VPN interface. Assign private sites to it and omit HTTP endpoints unless needed.

   Check for wildcard binding conflicts with the legacy listener. A private address binding alone doesn’t guarantee privacy—a WAN forwarding rule could still expose it.

   Access checks use nginx’s socket peer address. A local forwarder can make external clients appear local, so verify this before trusting a loopback allowlist.

4. Restrict sensitive application paths

   For public applications:
   - Use Blocked paths, with Include subpaths, for endpoints nobody should access.
   - Use Path access rules to limit administrative paths to admins.
   - Keep application authentication enabled.

   Blocking /admin protects that path and its subpaths, not undiscovered administrative endpoints.

5. Choose how certificates should use HTTP

   | Certificate method                                      | Public HTTP setting         |
   | ------------------------------------------------------- | --------------------------- |
   | All certificates use DNS-01; HTTP redirects unnecessary | Off                         |
   | HTTP-01 needs temporary reachability                    | Certificate validation only |
   | You intentionally retain HTTP-to-HTTPS redirects        | Always on with redirects    |

   DNS-01 avoids inbound HTTP validation; HTTP-01 requires public TCP port 80. Let’s Encrypt challenge documentation

   For validation-only mode, configure and test the WAN lifecycle. Externally managed does not change firewall rules. The managed IPv4 adapter still requires verification on your firmware and blocks WAN IPv6 port 80. After a renewal, check Certificates → HTTP access reports closure and test externally that port 80 is closed.

6. Verify enforcement

   Test from your LAN, VPN, and mobile data, covering IPv4 and IPv6. Confirm private sites and backend ports are inaccessible externally while intended public sites work. Check Ban Shield is enforcing rather than observing, and review legitimate-client bans before tightening thresholds.

Keep **Advanced global settings → Run nginx in background** enabled
(`nginx_daemon = true`) for init.d operation. An existing explicit `false`
survives the upgrade and must be changed; foreground nginx prevents startup
scripts from continuing. Saving Global settings renders the current embedded
templates, validates the combined nginx tree and reloads a running nginx.

## Listener and source boundaries

A listener profile owns literal HTTPS address/port pairs. Every profile needs at
least one HTTPS endpoint. Wildcard and specific bindings on the same port and
address family cannot belong to different profiles. The reserved `legacy`
profile uses the existing `router_ip`, `http_port`, and `https_port` settings;
adding a profile does not move existing sites. A subfolder inherits its domain's
listener. Subdomains select their own listener.

Access rules contain source IP addresses or CIDRs, including IPv6. Built-ins are
`public` (any source), `deny` (no source), and `legacy` (the existing allowlist).
An empty named rule denies everyone. A child cannot widen a parent's rule:
requests must satisfy both, including nested routes and subfolder applications.
A public subfolder therefore still needs an accessible containing domain.
Domain containers have no root application; configure application path rules in
each subfolder editor. Restricted Sessions uses `legacy` unless the advanced
`restricted_sessions_policy` selector assigns another rule.

Use **Blocked paths** for an application administrative endpoint. With **Include
subpaths**, `/admin` creates exact `/admin` and prefix `/admin/` blocks while
leaving `/administrator` available. This supplements application authorization;
it cannot identify undocumented alternative administrative URLs. Structured path
rows retain advanced properties and nested routes. Advanced route policies
remain available for regex, transport, cache and proxy settings.

Profiles and rule names are labels, not user identities. Source checks use the
socket peer address. A local TCP forwarder can make every user appear to be one
local address, sharing source-based access and rate budgets. Forwarded request
headers do not establish a trusted identity here. Separate listener bindings and
network enforcement are still necessary. Binding a VPN/private address requires
that address to exist and the firewall to permit it. Nonstandard HTTPS ports
must appear in client URLs; DNS alone does not convey a browser port. The proxy
replaces client forwarding headers with the validated hostname, actual HTTPS
scheme and listener port. Applications must honor those trusted values or have
their external base URL configured; a listener profile cannot fix an application
that constructs redirects using its own unrelated URL setting.

Unknown hosts are rejected on managed profiles, and unassigned TLS endpoints
have rejecting defaults. Authored custom nginx objects remain administrator
configuration: inspect their listeners, default servers and shared includes
before migrating. Maestro cannot promise their isolation. Validation errors
restore managed inputs, generated files and enabled links. In validation-only
mode, nginx validation also inventories expanded includes with `nginx -T`.
Authored non-TLS listeners on public port 80 or a configured challenge HTTP
port block adoption/renewal, including custom vhosts that could share the
challenge socket without causing a syntax error. Resolve the reported bindings
or retain Always on. Authored content is preserved; cleanup still removes managed
listeners and reports the unresolved custom exposure separately. References in defaults,
routes and disabled sites prevent deleting a rule/profile still in use.

For a staged move, the same hostname can be active on two listener profiles
while they share an upstream. Test the replacement listener before removing the
old site. Deleting the old site preserves the remaining site's upstream mapping.
An exact hostname reserved by a managed site is eligible for upstream lookup
only on its active profiles, even when another listener matches a wildcard.
If an older configuration has duplicate hostnames on the same profile, edit or
delete the conflicting object in the site editor before saving the full settings.

## HTTP-01 certificate windows

The shared **Public HTTP** mode controls all managed HTTP listeners:

- **Off** keeps them closed and rejects HTTP-01 issuance. DNS-01 still works.
- **Certificate validation only** opens challenge-only HTTP handling for actual
  scheduled/manual issuance or renewal, then closes it after the batch. Reviewing
  certificates, checking expiry, empty batches and DNS-only work do not open it.
- **Always on with redirects** retains configured redirects through renewals.

HTTP-01 names without an enabled redirect on the legacy listener receive a
challenge-only handler there. Ordinary requests to those names return `404`.

New configurations default to validation-only. Existing `nginx.yaml` inputs
without `http_access` retain always-on behavior until explicitly migrated.
Validation-only disables ordinary HTTP redirects. HTTP endpoints/redirects in a
profile cannot override the shared mode. Redirect profiles need an unambiguous
HTTPS target port.

For HTTP-01, select an eligible challenge listener and set its HTTP address/port.
The dehydrated and nginx challenge directories must match. nginx must be running.
The controller validates/reloads a challenge-only server, checks its readiness,
then opens owned WAN access. A single serialized session covers the HTTP jobs in
a batch and closes WAN access before removing nginx HTTP listeners. Individual
failures do not skip cleanup. Manual requests continue cleanup if the browser
disconnects; graceful shutdown cancels the managed process group and closes the
window. Concurrent settings saves cannot change active challenge bindings.

Public DNS must lead to reachable external TCP port 80. Private applications do
not need public vhosts for certificate validation. DNS-01 is appropriate when
public HTTP validation is impossible; wildcard certificates also need DNS-01.
After successful issuance or renewal, Maestro deploys standard dehydrated
output from `<certificates.certs_dir>/certs/<certificate-name>` into
`<nginx.root_dir>/certs/<certificate-name>`, validates nginx and reloads it when
running. Failed validation/reload restores the previous deployed files. Use the
certificate directory name expected by the hostname map (for example
`example_com`). Custom hooks that produce no standard output retain responsibility
for their own deployment. Verify the live fingerprint after migration.

TLS session caching and tickets are disabled for generated sites. Dynamic
certificate selection combined with shared session caching caused resumed TLS
handshakes to fail after configuration reloads; fresh handshakes avoid that
failure at the cost of additional handshake work.

The optional router adapter uses transient owned iptables/ip6tables chains and
an ipset lease. Configure the WAN interface and an IPv4 address actually assigned
to this host. It maps external TCP 80 to the selected internal challenge port,
and the lease authorizes only connections whose original destination port was 80.
The adapter protects router-local INPUT and guards against accidental forwarding to a LAN
host. It blocks WAN IPv6 TCP 80 rather than implementing IPv6 HTTP-01; use DNS-01
or externally managed dual-stack validation when published AAAA records require
it. Conflicting permanent port-80 forwarding must be resolved first; the adapter
does not edit saved firmware/NVRAM port-forward settings.

**The adapter has simulated command coverage, not target-router certification.**
Validate the firmware's actual packet path, external reachability/closure and
interaction with other rules in an isolated router lab before using it. Firewall
reconciliation runs every 15 seconds and on SIGUSR1. Arrange the firmware's
firewall post-hook to notify Maestro after rebuilding rules; periodic checks
cannot prevent a gap while owned hooks are missing. Detected rule damage closes
the session instead of extending its deadline.

External WAN management is an explicit alternative. Its forwarding and expiry
must be integrated and verified separately; selecting it does not toggle the
router's rules automatically. The Certificates view shows the actual lifecycle
state/deadline and provides retry closure after failures. An error means exposure
may remain; inspect both router rules and nginx rather than trusting the saved
mode.

The maximum batch window defaults to 3600 seconds (60–86400 configurable).
Per-command timeouts are capped by the remaining deadline. A timed kernel ipset
membership bounds managed WAN acceptance while its hooks remain installed.
**Local nginx listener closure after abrupt process death is deferred until
startup recovery/manual cleanup.** A timer in the dead process cannot close it.
Startup attempts stale WAN/listener cleanup before new renewal work. These
mechanisms do not constitute a guarantee of crash-safe closure on arbitrary
firmware or with external forwarding.

## Stored inputs and API

Named collections and `http_access` are top-level tables in
`<nginx.inputs_dir>/nginx.yaml`, alongside `global` and `site_defaults`.
See the synthetic [global inputs](../config/nginx/examples/ingress.yaml) and
[site inputs](../config/nginx/examples/ingress-sites.yaml). Replace example
addresses and paths before use; saving through the UI handles these tables.

Authenticated API clients use the existing `/api/nginx/settings` and object
endpoints. `POST /api/nginx/access-preview` resolves a draft without applying it;
`GET /api/nginx/ingress-usage` reports collection references. Certificate window
status is `GET /api/certificates/http-access`; `DELETE` retries closure when no
certificate operation owns it. Preserve new fields in read/write round trips;
unsafe old-client replacement requests are rejected rather than deleting
referenced settings.
