# A small AArch64 Maestro lab

> Historical lab proposal. For maintained provisioning steps and upgrade compatibility, see [the active lab guide](../../../lab/aarch64/README.md).

**Proposed outcome.** Boot one persistent Alpine Linux guest in QEMU, install the standard Maestro AArch64 IPK with Entware, and open the generated router page at `http://127.0.0.1:8080/user20.asp`. From that page, an operator can turn a real WireGuard interface on and off. Maestro uses its normal Asuswrt configuration, command runner, package hooks, and service script. All lab-specific behavior belongs to the guest. This plan has not been boot-tested.

## Why Alpine fits this lab

Keep Alpine for the first image. Each executable runs as its own process on the guest kernel, so the choice of libc depends on which executable the lab is exercising.

| Component | Runtime used in the lab |
| --- | --- |
| Maestro IPK | The release targets static AArch64 musl. Inspection of the local release binary with `file` and `readelf -lW -dW` showed an AArch64 executable with no ELF interpreter or dynamic section. It needs no guest libc to start. [Release packaging](../../releases.md) |
| Entware tools | The AArch64 feed builds against glibc and supplies its loader and libraries under `/opt`. Install `wg` and `wg-quick` from the same feed used on the router. The script runs its interpreter and invokes the tools as separate processes. [Entware target configuration](https://github.com/Entware/Entware/blob/master/configs/aarch64-3.10.config) |
| Guest and simulated firmware | Alpine supplies musl, BusyBox httpd and the shell for the NVRAM mock. These exercise the HTTP and command interfaces. WireGuard itself uses the guest kernel. [Alpine userspace](https://alpinelinux.org/about/) |

The choice would change for a real firmware executable such as ASUS httpd. Merlin documents a Broadcom HND AArch64 toolchain using glibc 2.22, and toolchains vary by model and firmware. For that milestone, identify the exact firmware build and inspect the executable with `readelf -hW -lW -dW` for its architecture, interpreter path and required libraries. Use the matching firmware userspace and vendor libraries for binary-level testing. A generic glibc distribution alone cannot reproduce the vendor dependencies. [Merlin toolchain notes](https://github.com/RMerl/asuswrt-merlin.ng/blob/main/README.TXT)

During the initial guest setup, running `/opt/bin/opkg print-architecture` and `/opt/bin/wg --version` exercises the installed tools directly. Keep Alpine and Entware libraries in their package-managed locations and leave the global `LD_LIBRARY_PATH` unset.

## 1. Boot Linux once

Use an official stable Alpine AArch64 **Standard** ISO and its published checksum. On the host install `qemu-system-aarch64`, `qemu-img`, and the distribution's AArch64 EDK II/UEFI firmware. Use QEMU's generic `virt` board. Alpine supplies the kernel and OpenRC. Neither a firmware build nor a Rust toolchain is needed to install a published Maestro package. [Alpine downloads](https://alpinelinux.org/downloads/) · [QEMU Arm virt](https://www.qemu.org/docs/master/system/arm/virt.html)

The implemented `make init-lab` caches the ISO and persistent disk in ignored
`data/lab/`; see the [lab guide](../../../lab/aarch64/README.md) for setup and
path overrides. The host launcher, `lab/aarch64/run.sh`, accepts a disk location
and optional installer ISO. Normal boots reuse the existing disk. The basic
QEMU arguments are:

```sh
qemu-system-aarch64 -machine virt -cpu cortex-a53 -smp 2 -m 1024 -nographic \
  -bios "$AARCH64_UEFI" \
  -drive file=router-lab.qcow2,format=qcow2,if=virtio \
  -nic user,model=virtio-net-pci,hostfwd=tcp:127.0.0.1:8080-:8080,hostfwd=tcp:127.0.0.1:8443-:8443,hostfwd=tcp:127.0.0.1:3030-:3030
```

Use the host's QEMU-compatible AArch64 firmware blob for `AARCH64_UEFI`. Hosts packaging split flash images should attach the read-only firmware code and a per-VM writable copy of its matching variables image as pflash drives. On the install boot, also attach the ISO via a virtio SCSI controller and CD drive:

```sh
-device virtio-scsi-pci -drive if=none,id=cd,media=cdrom,readonly=on,file="$ALPINE_ISO" \
-device scsi-cd,drive=cd,bootindex=1
```

On the serial console, run `setup-alpine`, choose DHCP for the guest NIC and a **sys** install on the VM's empty `vda`. Shut down, remove the installer arguments and boot the installed disk. Use one NIC with QEMU's default user network throughout. Its first DHCP address is normally `10.0.2.15`. The three loopback forwards give the host browser access to HTTP, HTTPS and the Maestro API. [QEMU user networking](https://www.qemu.org/docs/master/system/devices/net.html)

## 2. Provision the guest, then hand service startup to Entware

The guest-side `lab/aarch64/provision.sh` reads scripts, fixtures and IPKs from
a read-only QEMU 9p project share. Normal disk boots export the checkout using
`-virtfs "local,path=$REPO_DIR,mount_tag=maestro,security_model=none,readonly=on"`.
After the Alpine disk is configured, restart with the launcher and run as root
inside the guest:

```sh
modprobe 9pnet_virtio
mkdir -p /mnt/maestro
mount -t 9p -o trans=virtio,version=9p2000.L,ro maestro /mnt/maestro
sh /mnt/maestro/lab/aarch64/provision.sh
```

Provisioning selects the newest IPK by modification time in the checkout's
`dist/` directory, verifies its matching checksum, and installs it on the guest
disk. Set `PACKAGE` to the absolute shared IPK path to select another version.
Provisioning records the mount in `/etc/fstab` and installs a module-load entry
for subsequent boots. Installer ISO boots also export the share so the Alpine
answer file can be read during installation.

1. Install Alpine's `ca-certificates`, `curl`, `openssl`, `iproute2`, and `busybox-extras`. Create persistent directories `/opt`, `/jffs/configs`, `/jffs/scripts`, and `/www/require/modules` on the guest disk.
2. If `/opt/bin/opkg` is absent on a fresh guest, download the AArch64 installer with `curl -fLo /tmp/entware.sh https://bin.entware.net/aarch64-k3.10/installer/generic.sh` and run `/bin/sh /tmp/entware.sh`. Confirm `/opt/bin/opkg print-architecture` includes `aarch64-3.10`. Include `/opt/bin` and `/opt/sbin` in the guest shell's PATH. [Entware installer](https://github.com/Entware/installer.sh/blob/master/generic.sh)
3. Use one executable OpenRC service at `/etc/init.d/entware` with the body below. Enable it with `rc-update add entware default`. The packaged Maestro service will be discovered on subsequent boots. The Maestro IPK itself starts the service on first install. [Packaged service script](../../../scripts/S99maestro)

```sh
#!/sbin/openrc-run
depend() { need localmount net; }
start() {
    mkdir -p /tmp/var/wwwext || return 1
    /opt/etc/init.d/rc.unslung start
}
stop() { /opt/etc/init.d/rc.unslung stop; }
```

## 3. Supply the firmware calls Maestro actually makes

Install a root-owned executable at `/bin/nvram`. The service calls `nvram get lan_ipaddr` while resolving AdGuard settings. The optional diagnostics collector also reads five identification keys. Put values in one editable guest file, `/etc/router-lab/nvram`, so changing the simulated router identity takes effect on the next read. At provisioning time seed `lan_ipaddr` with the guest NIC's actual IPv4 address. With default QEMU networking, an example file is:

```ini
lan_ipaddr=10.0.2.15
productid=QEMU-AARCH64-LAB
odmpid=router-lab
firmver=lab
buildno=1
extendno=qemu
```

Use the following POSIX shell implementation. A missing key gives an empty value, an unsupported operation fails, and fixture contents are read as data:

```sh
#!/bin/sh
if [ "$#" -ne 2 ] || [ "$1" != get ]; then
    echo 'usage: nvram get KEY' >&2
    exit 2
fi
awk -v key="$2" '
    index($0, key "=") == 1 { print substr($0, length(key) + 2); found=1; exit }
    END { if (!found) print "" }
' /etc/router-lab/nvram
```

On Asuswrt, leaving `adguard.lan_ip` empty exercises this path even when the external AdGuard integration is disabled. A useful spot check is to compare `/bin/nvram get lan_ipaddr` with the address returned by the authenticated `GET /api/adguard/config`. [LAN lookup](../../../src/adguard.rs) · [AdGuard handlers](../../../src/api/adguard.rs) · [Collector keys](../../../scripts/collect-logs.sh)

The other firmware action in these handlers is exactly `service dnsmasq restart`, after Maestro writes `hosts.add` or `dnsmasq.conf.add`. Leave Alpine's own service manager intact. Set Maestro's existing `commands.service` option to `/usr/local/libexec/router-lab/service`, with this executable as its target:

```sh
#!/bin/sh
if [ "$#" -eq 2 ] && [ "$1" = dnsmasq ] && [ "$2" = restart ]; then
    exec logger -t router-lab 'mock: dnsmasq restart requested'
fi
echo 'unsupported router service action' >&2
exit 2
```

This records a restart request so the UI's file-edit workflow can complete. DNS answers require a real dnsmasq integration in a later increment. Normal Entware service actions already invoke the real init scripts. For the service log fallback, point `commands.logread` at a working guest syslog reader taking no arguments. If the chosen Alpine syslog setup logs only to a file, use a small executable that reads the last 200 lines of `/var/log/messages`. [Service logs](../../../src/api/services.rs)

## 4. Add real WireGuard and a small web root

**WireGuard.** Run `/opt/bin/opkg update` and `/opt/bin/opkg install wireguard-tools wg-quick`. Confirm the guest kernel supports creating a WireGuard link with `ip link add wgprobe type wireguard`, then remove it with `ip link del wgprobe`. If the interface type is missing, install the matching Alpine kernel/modules with WireGuard support before proceeding. Entware supplies the userspace commands. [Entware AArch64 package index](https://bin.entware.net/aarch64-k3.10/Packages.html)

Create `/opt/etc/wireguard/wgs1.conf` once, under `umask 077`. Give it an `[Interface]` section with a unique `PrivateKey` generated by `/opt/bin/wg genkey` and `Address = 10.77.0.1/24` if that subnet is unused. Leave the interface down for the UI to start. Maestro calls `wg-quick up /opt/etc/wireguard/wgs1.conf`, `wg show wgs1`, and `wg-quick down wgs1`. Check that the installed wg-quick resolves the short name to that same file. If its compiled search path differs, symlink the file into that directory. A second VM or VPN peer is unnecessary for this first milestone. [WireGuard handler](../../../src/api/wireguard.rs)

**httpd.** Use Alpine's BusyBox extras httpd applet. Have a small OpenRC service supervise `/bin/busybox-extras httpd -f -p 8080 -h /www -c /etc/router-lab/httpd.conf`. Enable that service in the default runlevel. Add a separate stunnel OpenRC service on HTTPS port 8443 forwarding to HTTP 8080, with a guest-generated self-signed certificate. Leave guest ports 80 and 443 for Entware NGINX. Set the config's MIME mapping to `.asp:text/html`. Protect the whole root with a `/:lab:<password>` Basic auth rule populated with a random guest-only password at provisioning time. Store the config outside the web root with mode 0600. The rendered ASP contains the API token. [Alpine BusyBox applet configuration](https://github.com/alpinelinux/aports/blob/master/main/busybox/busyboxconfig-extras) · [Page renderer](../../../src/asus_ui.rs)

Seed `/www/require/modules/menuTree.js` with the following writable fixture. Preserve its line breaks because the current menu editor matches them:

```js
var menuList = [
{
menuName: "Lab",
index: "menu_Alexa_IFTTT",
tab: []
},
];
```

Put `function show_menu() {}` in `/www/state.js` to satisfy the template's onload call. Link `/www/user20.asp` to `/tmp/var/wwwext/user20.asp`. Maestro writes the target and edits the menu fixture at startup. This provides a working Maestro page in a minimal shell. ASUS styling, menu navigation, and firmware login can be evaluated on hardware when needed. [ASP template](../../../config/asus-wrt.asp.template)

## 5. Install the ordinary Maestro package

Prepare the settings **before** installing the IPK. Its post-install hook runs migration, initializes nginx files, creates the JFFS hooks, validates config, and restarts Maestro immediately. On a clean guest, initialization needs real OpenSSL to generate a fallback certificate. The renderer is embedded in the binary, so running an nginx daemon or installing Python/Jinja2 is unnecessary for this milestone. [Package setup](../../../scripts/setup-merlin.sh) · [nginx bootstrap](../../../src/nginx_render.rs)

Create `/opt/etc/maestro/maestro.toml` once with mode 0600. Rely on the application's defaults for the rest. The proposed provisioning script should generate this TOML using the actual values below:

| Setting | Guest value |
| --- | --- |
| Top level | `platform = "asuswrt"`, `test_mode = false` |
| `server` | Bind `0.0.0.0` on port `3030`. Generate `auth_token` once with `openssl rand -hex 24`. Allow the exact browser origins `http://127.0.0.1:8080`, `https://127.0.0.1:8443` and `http://127.0.0.1:3030`. |
| `asus_ui` | Keep enabled, retain the default menu tree and rendered page, and leave `api_base_url` empty. The page uses the browser hostname and API port. |
| `paths` | Set `wireguard_conf = "/opt/etc/wireguard/wgs1.conf"`. |
| `commands` | Use the mock service path above, the guest syslog reader, and the absolute path to Alpine OpenSSL. Check `wg`, `wg_quick`, and `ip` against the installed executable locations. |
| `adguard` | Set `enabled = false` and `lan_ip = ""` for NVRAM discovery without an external server. |

On a fresh disk, seed `firewall-policy.json` with `{"enabled":false,"rules":[]}` in both `/opt/etc/maestro` and `/opt/var/lib/maestro`. This keeps the package helper from populating the active example policy. Preserve existing files when reprovisioning. Then, in the guest directory containing the release IPK and its matching checksum, run:

```sh
sha256sum -c "$PACKAGE.sha256" && /opt/bin/opkg install "./$PACKAGE"
```

Here `PACKAGE` is the downloaded IPK's basename. To try local changes later, use the existing AArch64 release build and IPK packaging scripts and transfer their output through the same path. The guest boot and service configuration stay the same. [Installation guide](../../installation.md) · [IPK builder](../../../scripts/package-ipk.sh)

## 6. One acceptance walkthrough, then extend as needed

Reboot the VM. Open `http://127.0.0.1:8080/user20.asp`, sign in to httpd, and confirm Maestro shows live system metrics. Turn WireGuard on in the UI and confirm `/opt/bin/wg show wgs1` reports the interface. Turn it off and confirm the interface disappears. This exercises the release binary, persistence, Entware startup, httpd, browser authentication, and a real kernel action. The NVRAM spot check above is sufficient for the mock.

For the next feature, install or configure it in the running guest first. When it works, add its package and any required fixture to the same provisioner. Keep settings, generated credentials, WireGuard keys and edited state on the disk across runs. For example, adding real DNS means installing dnsmasq, configuring it to consume the two JFFS files, and replacing the single logged restart with the real service action. A clean shutdown and a copy of the qcow2 disk provide a convenient rollback point before such changes. Hardware and proprietary firmware behavior remain a separate router validation step.
